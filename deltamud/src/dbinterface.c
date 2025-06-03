#include "conf.h"
#include "sysdep.h"
#include <stdarg.h>

#include "structs.h"
#include "utils.h"
#include "db.h"
#include "handler.h"
#include "dbinterface.h"
#include "comm.h"

/* MySQL Database Connection Routines */
void connect_database (void) {
  char *env_user, *env_pass;
  
  SQLdb = (MYSQL *) malloc (sizeof(MYSQL));

  mysql_init(SQLdb);

  /* Get MySQL credentials from environment variables */
  env_user = getenv("MYSQL_USER");
  env_pass = getenv("MYSQL_PASSWORD");
  
  /* Fallback to compiled defaults if environment variables not set */
  if (!env_user) env_user = (char *)mySQL_user;
  if (!env_pass) env_pass = (char *)mySQL_pass;
  
  /* Exit with error if no credentials available */
  if (!env_user || !env_pass) {
    log("SYSERR: MySQL credentials not found in environment variables MYSQL_USER and MYSQL_PASSWORD");
    exit(1);
  }

  if (!mysql_real_connect(SQLdb, "127.0.0.1", env_user, env_pass, "deltamud", mySQL_port, NULL, 0)) {
    sprintf(buf, "Could not connect to mySQL host database server: %s", mysql_error(SQLdb));
    log (buf);
    exit(0);
  }
}

/* Stormilicious (c) 2000
   MySQL Database Server Playerfile Insertion/Retrieval Routines, Manipulation HOWTO
   When you add a new element to the character structures and you want it to save, you have
   to do the following:
   1. Increment or decrement (if you're adding a saving element or removing one, respectively)
      NUM_PLAYERS_MAIN_ROW_ELEMENTS by the appropriate number (for fools that don't get it:
      if you're adding an element, add 1 to NUM_PLAYERSetc.)
   2. Add a column into the MySQL player_main table and name it appropriatley (I don't know how
      to do this myself yet, so you're on your own).
   3. In the init_querystring function, add:
      COLUMN_[type]("[column_name]",[pointer]);
      type is NRM for numeric variables, and STR for strings.
      column_name is the name of the column you added to the player_main table.
      pointer is the pointer defined off of struct char_data *ch.

      COLUMN_NULL is provided for compatibility, it accepts a column name as it's only argument -
      use this when you have a column and don't want it to be loaded in on a player entry retrieve.

      Examples:
      COLUMN_NRM("idnum",ch->char_specials.saved.idnum);
      ^^ I'm adding a numeric saving value, which is saved in the idnum column of my table, and is referenced
         in the code by GET_IDNUM(ch).

      COLUMN_STR("name",ch->player.name);
      ^^ I'm adding a string saving value, which is saved in the name column of my table, and is referenced
         in the code by GET_NAME(ch) *or* ch->player.name.


      Order does NOT matter (as it did in earlier versions), you may place your COLUMN_x declarations
      any way you want, however, the amount of column entries SHOULD EQUAL NUM_PLAYERS_MAIN_ROW_ELEMENTS,
      and as a good rule of thumb you should have every column (even if it isn't loaded) entered into the list
      (if you read above, ones that aren't loaded are denoted COLUMN_NULL).

*/

#define NUM_PLAYER_MAIN_ROW_ELEMENTS 83 

#define COLUMN_INCR (++i)

#define COLUMN_NRM(query,element)  \
  if (mode==0) { \
    if (querystring[COLUMN_INCR]) \
      free(querystring[i]); \
    querystring[i]=strdup(query); \
    structurevarsize[i]=sizeof(element); \
  } \
  else { \
    COLUMN_INCR; \
    columnptrs[i].ptr=(long *) &(element); \
    columnptrs[i].type=0; \
  } \

#define COLUMN_STR(query,element)  \
  if (mode==0) { \
    if (querystring[COLUMN_INCR]) \
      free(querystring[i]); \
    querystring[i]=strdup(query); \
    structurevarsize[i]=sizeof(element); \
  } \
  else { \
    COLUMN_INCR; \
    columnptrs[i].ptr=(long *) &(element); \
    columnptrs[i].type=1; \
  } \

#define COLUMN_NULL(query)  \
  if (mode==0) { \
    if (querystring[COLUMN_INCR]) \
      free(querystring[i]); \
    querystring[i]=strdup(query); \
    structurevarsize[i]=0; \
  } \
  else { \
    COLUMN_INCR; \
    columnptrs[i].ptr=NULL; \
    columnptrs[i].type=-1; \
  } \

char *querystring[NUM_PLAYER_MAIN_ROW_ELEMENTS];
char structurevarsize[NUM_PLAYER_MAIN_ROW_ELEMENTS];
#define TYPECAST(i, x) (structurevarsize[i] == 1 ? (byte) x : (structurevarsize[i] == 2 ? (short int) x : structurevarsize[i] == 4 ? (long) x : (long) x ))

struct {
  long *ptr;
  char type; /* 0 for normals, 1 for strings. */
} columnptrs[NUM_PLAYER_MAIN_ROW_ELEMENTS];

char *char_last_host=NULL;

void QUERY_DATABASE(MYSQL *db, char *query, int len) {
  static char tries=0;
  if (mysql_real_query(db, query, len)) { /* This is BAD! */
    if (tries>=2) {
      log("ERROR: Tried twice, MySQL server appears to be down. Cannot continue, giving up.");
      exit(0);
    }
    tries++;
    if (tries==1) {
      log("ERROR: Query to MySQL database failed to process! Resetting connection to server and retrying ...");
      log("       Failed query follows:");
      query[len]=0; /* Just to make sure we don't buffer overflow. */
      log(query);
    }
    else
      log("ERROR: Attempt failed, trying again ...");
    mysql_close(SQLdb);			      /* Whether the DB connection died or something else... reset the connection and try again. */
    connect_database();
    QUERY_DATABASE(db, query, len); /* Retry. */
  }
  tries=0;
}

MYSQL_RES *STORE_RESULT (MYSQL *db) {
  MYSQL_RES *result;
  if (!(result=mysql_store_result(db))) { /* If this is NULL (and we're SELECTING) we have an error. */
    sprintf(buf, "ERROR: Could not store result: %s", mysql_error(db));
    log(buf);
    return NULL;
  }
  return result;
}

MYSQL_ROW FETCH_ROW (MYSQL_RES *result) {
  MYSQL_ROW row;
  if (!(row=mysql_fetch_row(result)) && mysql_errno(SQLdb)) { /* Server must of died.. PANIC! */
    sprintf(buf, "ERROR: While attempting to fetch row: %s", mysql_error(SQLdb));
    log(buf);
    return NULL;
  }
  return row;
}

void init_querystring (struct char_data *ch, int mode) {
      int i=-1;
      /* These can be in any order, just make sure you have the right column name string
         matching with it's appropriate pointer on the character. */
      COLUMN_NRM("idnum",ch->char_specials.saved.idnum);
      COLUMN_STR("name",ch->player.name);
      COLUMN_STR("description",ch->player.description);
      COLUMN_STR("title",ch->player.title);
      COLUMN_NRM("sex",ch->player.sex);
      COLUMN_NRM("class",ch->player.class);
      COLUMN_NRM("race",ch->player.race);
      COLUMN_NRM("deity",ch->player.deity);
      COLUMN_NRM("level",ch->player.level);
      COLUMN_NRM("hometown",ch->player.hometown);
      COLUMN_NRM("birth",ch->player.time.birth);
      COLUMN_NRM("played",ch->player.time.played);
      COLUMN_NRM("weight",ch->player.weight);
      COLUMN_NRM("height",ch->player.height);
      COLUMN_STR("pwd",ch->player.passwd);

      COLUMN_NRM("last_logon", ch->player.time.logon);
      COLUMN_STR("host", char_last_host);

      COLUMN_NRM("mana",ch->points.mana);
      COLUMN_NRM("max_mana",ch->points.max_mana);
      COLUMN_NRM("hit",ch->points.hit);
      COLUMN_NRM("max_hit",ch->points.max_hit);
      COLUMN_NRM("move",ch->points.move);
      COLUMN_NRM("max_move",ch->points.max_move);

      COLUMN_NRM("gold",ch->points.gold);
      COLUMN_NRM("bank_gold",ch->points.bank_gold);
      COLUMN_NRM("exp",ch->points.exp);
      COLUMN_NRM("power",ch->points.power);
      COLUMN_NRM("mpower",ch->points.mpower);
      COLUMN_NRM("defense",ch->points.defense);
      COLUMN_NRM("mdefense",ch->points.mdefense);
      COLUMN_NRM("technique",ch->points.technique);

      COLUMN_NRM("str",ch->real_abils.str);
      COLUMN_NRM("str_add",ch->real_abils.str_add);
      COLUMN_NRM("intel",ch->real_abils.intel);
      COLUMN_NRM("wis",ch->real_abils.wis);
      COLUMN_NRM("dex",ch->real_abils.dex);
      COLUMN_NRM("con",ch->real_abils.con);
      COLUMN_NRM("cha",ch->real_abils.cha);

      COLUMN_NRM("PADDING0",ch->player_specials->saved.PADDING0);
      COLUMN_NRM("talks1",ch->player_specials->saved.talks[0]);
      COLUMN_NRM("talks2",ch->player_specials->saved.talks[2]);
      COLUMN_NRM("talks3",ch->player_specials->saved.talks[3]);
      COLUMN_NRM("wimp_level",ch->player_specials->saved.wimp_level);
      COLUMN_NRM("freeze_level",ch->player_specials->saved.freeze_level);
      COLUMN_NRM("invis_level",ch->player_specials->saved.invis_level);
      COLUMN_NRM("load_room",ch->player_specials->saved.load_room);
      COLUMN_NRM("pref",ch->player_specials->saved.pref);
      COLUMN_NRM("bad_pws",ch->player_specials->saved.bad_pws);
      COLUMN_NRM("cond1",ch->player_specials->saved.conditions[0]);
      COLUMN_NRM("cond2",ch->player_specials->saved.conditions[1]);
      COLUMN_NRM("cond3",ch->player_specials->saved.conditions[2]);
      COLUMN_NRM("death_timer",ch->player_specials->saved.death_timer);
      COLUMN_NRM("citizen",ch->player_specials->saved.citizen);
      COLUMN_NRM("training",ch->player_specials->saved.training);
      COLUMN_NRM("newbie",ch->player_specials->saved.newbie);
      COLUMN_NRM("arena",ch->player_specials->saved.arena);
      COLUMN_NRM("spells_to_learn",ch->player_specials->saved.spells_to_learn);
      COLUMN_NRM("questpoints",ch->player_specials->saved.questpoints);
      COLUMN_NRM("nextquest",ch->player_specials->saved.nextquest);
      COLUMN_NRM("countdown",ch->player_specials->saved.countdown);
      COLUMN_NRM("questobj",ch->player_specials->saved.questobj);
      COLUMN_NRM("questmob",ch->player_specials->saved.questmob);
      COLUMN_NRM("recall_level",ch->player_specials->saved.recall_level);
      COLUMN_NRM("retreat_level",ch->player_specials->saved.retreat_level);
      COLUMN_NRM("trust",ch->player_specials->saved.trust);
      COLUMN_NRM("bail_amt",ch->player_specials->saved.bail_amt);
      COLUMN_NRM("wins",ch->player_specials->saved.wins);
      COLUMN_NRM("losses",ch->player_specials->saved.losses);
      COLUMN_NRM("pref2",ch->player_specials->saved.pref2);
      COLUMN_NRM("godcmds1",ch->player_specials->saved.godcmds1);
      COLUMN_NRM("godcmds2",ch->player_specials->saved.godcmds2);
      COLUMN_NRM("godcmds3",ch->player_specials->saved.godcmds3);
      COLUMN_NRM("godcmds4",ch->player_specials->saved.godcmds4);
      COLUMN_NRM("clan",ch->player_specials->saved.clan);
      COLUMN_NRM("clan_rank",ch->player_specials->saved.clan_rank);
      COLUMN_NRM("mapx",ch->player_specials->saved.mapx);
      COLUMN_NRM("mapy",ch->player_specials->saved.mapy);
      COLUMN_NRM("buildmodezone",ch->player_specials->saved.buildmodezone);
      COLUMN_NRM("buildmoderoom",ch->player_specials->saved.buildmoderoom);
      COLUMN_NRM("tloadroom",ch->player_specials->saved.tloadroom);

      COLUMN_NRM("alignment",ch->char_specials.saved.alignment);
      COLUMN_NRM("act",ch->char_specials.saved.act);
      COLUMN_NRM("affected_by",ch->char_specials.saved.affected_by);
      if (i+1!=NUM_PLAYER_MAIN_ROW_ELEMENTS) { /* Goddamnit, you screwed up fool. */
        sprintf(buf, "ERROR: Number of elements in init_querystring (%d) don't match hard-coded number in NUM_PLAYER_MAIN_ROW_ELEMENTS (%d).", i+1, NUM_PLAYER_MAIN_ROW_ELEMENTS);
        log(buf);
        exit(0);
      }
}

void setzero (void *pointer, int size) {
  int i;
  char *ptr=(char *) pointer;
  if (size <= 0) return;
  for (i=0; i<size; i++)
    *(ptr+i) = 0;
}

int querybuild=0;

#define MODE_INSERT 1
#define MODE_UPDATE 2

/* This uses buf1. */
char * mkqueryset (char *query) {
  sprintf(buf1, ",%s=", query);
  return buf1;
}

#define MODE_DELETE 1
#define MODE_STORE 2
#define MODE_RETRIEVE 3

/* Affect/skill loading/storing. */

void dbmodify_player_affects (struct char_data *ch, int mode) { /* mode = 0 for delete, = 1 for store, = 2 for retrieve */
  struct affected_type *aff, af;
  MYSQL_RES *result;
  MYSQL_ROW row;

  if (mode != MODE_RETRIEVE) { /* Delete em, unless we're retrieving. */
    sprintf(buf, "DELETE from player_affects WHERE idnum=%ld", GET_IDNUM(ch));
    QUERY_DATABASE(SQLdb, buf, strlen(buf));
  }
  if (!mode) return;
  if (mode == MODE_STORE) { /* Do some storage. */
    aff=ch->affected;
    sprintf(buf, "INSERT into player_affects (idnum,type,duration,modifier,location,bitvector) VALUES");
    while (aff) {
      sprintf(buf+strlen(buf), "%s%ld,%ld,%ld,%ld,%d,%ld)", aff==ch->affected ? "(" : ",(", GET_IDNUM(ch), aff->type, aff->duration, aff->modifier, aff->location, aff->bitvector);
      aff=aff->next;
    }
    if (buf[strlen(buf)-1]==')')
      QUERY_DATABASE(SQLdb, buf, strlen(buf));
  }
  else if (mode == MODE_RETRIEVE) { /* Do some retrieval. */
    sprintf(buf, "SELECT type,duration,modifier,location,bitvector from player_affects WHERE idnum=%ld", GET_IDNUM(ch));
    QUERY_DATABASE(SQLdb, buf, strlen(buf));

    if (!(result=STORE_RESULT(SQLdb))) return;
    
    while ((row=FETCH_ROW(result))) { 
      af.type=ATOIROW(0);
      af.duration=ATOIROW(1);
      af.modifier=ATOIROW(2);
      af.location=ATOIROW(3);
      af.bitvector=ATOIROW(4);
      	affect_to_char (ch, &af);
    }
    mysql_free_result(result);
  }
}
// This provides storage space for 1024 spells, we currently have 1000.
long spellinfo[32]; /* Which spells are active, which are not. */

#define SP_SET_BIT(spellnum) (SET_BIT(spellinfo[(spellnum+1)/32], 1 << MAX_SKILLS%(spellnum+1)))
#define SP_IS_SET(spellnum) (IS_SET(spellinfo[(spellnum+1)/32], 1 << MAX_SKILLS%(spellnum+1)))

void dbmodify_player_skills (struct char_data *ch, int mode) { /* mode = 0 for delete, = 1 for store, = 2 for retrieve */
  int i, skillnum;
  static int cache=0;
  MYSQL_RES *result;
  MYSQL_ROW row;
  extern char *spells[];
  int fp=0;

  if (!cache) {
    cache=1;
    for (i=0; i<MAX_SKILLS; i++)
      if (strcmp(spells[i], "!UNUSED!"))
        SP_SET_BIT(i);
  }

  if (mode != MODE_RETRIEVE) { /* Delete em, unless we're retrieving. */
    sprintf(buf, "DELETE from player_skills WHERE idnum=%ld", GET_IDNUM(ch));
    QUERY_DATABASE(SQLdb, buf, strlen(buf));
  }
  if (!mode) return;
  if (mode == MODE_STORE) { /* Do some storage. */
    sprintf(buf, "INSERT into player_skills (idnum,skill,learned) VALUES");
    for (i=0; i<MAX_SKILLS; i++)
      if (SP_IS_SET(i) && ch->player_specials->saved.skills[i]>0) {
        sprintf(buf+strlen(buf), "%s%ld,%d,%d)", !fp ? "(" : ",(", GET_IDNUM(ch), i, ch->player_specials->saved.skills[i]);
        if (!fp) fp=1;
      }
    if (buf[strlen(buf)-1]==')')
      QUERY_DATABASE(SQLdb, buf, strlen(buf));
  }
  else if (mode == MODE_RETRIEVE) { /* Do some retrieval. */
    sprintf(buf, "SELECT idnum,skill,learned from player_skills WHERE idnum=%ld", GET_IDNUM(ch));
    QUERY_DATABASE(SQLdb, buf, strlen(buf));

    if (!(result=STORE_RESULT(SQLdb))) return;
    
    while ((row=FETCH_ROW(result)))
      if ((skillnum=ATOIROW(1)) >= 0 && skillnum <= MAX_SKILLS)
        ch->player_specials->saved.skills[skillnum]=ATOIROW(2);
    mysql_free_result(result);
  }
}

int delete_player_entry (int idnum) {
  int rows;
  sprintf(buf, "DELETE from player_main WHERE idnum='%d'", idnum);
  QUERY_DATABASE(SQLdb, buf, strlen(buf));

  if ((rows=mysql_affected_rows(SQLdb)) != 1) {
    if (!rows) {
      sprintf(buf, "ERROR: Could not find idnum '%d' for DELETE operation.", idnum);
      log(buf);
      return 0;
    }
    sprintf(buf, "ERROR: While executing DELETE operation on idnum '%d', deleted %d matching rows!", idnum, rows);
    log(buf);
  }
  sprintf(buf, "DELETE from player_affects WHERE idnum='%d'", idnum);
  QUERY_DATABASE(SQLdb, buf, strlen(buf));
  sprintf(buf, "DELETE from player_skills WHERE idnum='%d'", idnum);
  QUERY_DATABASE(SQLdb, buf, strlen(buf));

  return 1;
}

/* Benchmarking routines used for debugging.
struct timeval ts,te;
char tmpbuf[100];
#define time_start (gettimeofday(&ts, NULL))
#define time_end \
gettimeofday(&te, NULL); \
sprintf(tmpbuf, "%ld.%6lds\r\n", te.tv_sec-ts.tv_sec, te.tv_usec-ts.tv_usec); \
if (ch->desc) send_to_char(tmpbuf,ch); \
*/

/* This uses buf and buf2. */
int insert_player_entry (struct char_data *ch) {
  int i, fpass=0;
  int mode=0;
  MYSQL_RES *result;
  struct obj_data *char_eq[NUM_WEARS];
  char tmpstr[300];

  if (!ch || !ch->player_specials) return 0;
  
  /* Temporary fix: Skip saving if gold values are corrupted */
  if (ch->points.gold > 1000000000 || ch->points.gold < -1000000000 ||
      ch->points.bank_gold > 1000000000 || ch->points.bank_gold < -1000000000) {
    log("WARNING: Skipping save due to corrupted character data (gold values)");
    return 1; /* Return success to avoid crashes */
  }

  if (!querybuild) {
    init_querystring(ch, 0);
    querybuild=1;
  }

  /* Store all affects ... */
  dbmodify_player_affects(ch, MODE_STORE);
  dbmodify_player_skills(ch, MODE_STORE); /* Lets store the skills while we're at it. */
  sprintf(buf, "SELECT idnum FROM player_main WHERE name='%s'", GET_NAME(ch));
  QUERY_DATABASE(SQLdb, buf, strlen(buf));
  if (!(result=STORE_RESULT(SQLdb))) return 0;

  /* Remove all EQ affects ... We need to write only bare character data. */
  for (i = 0; i < NUM_WEARS; i++)
    {
      if (GET_EQ (ch, i))
	char_eq[i] = nunequip_char (ch, i);
      else
	char_eq[i] = NULL;
    }

  /* Remove all other affects from memory ... */
  while (ch->affected) affect_remove (ch, ch->affected);
  /* Update player time data. */
  ch->player.time.played+=(long) (time (0) - ch->player.time.logon);
  ch->player.time.logon = time (0);

  if (char_last_host) free(char_last_host);
  if (ch->desc && ch->desc->host) char_last_host=strdup(ch->desc->host);
  else char_last_host=NULL;

  if (mysql_num_rows(result)==0)
    mode=MODE_INSERT;
  else
    mode=MODE_UPDATE;

  mysql_free_result(result);

  if (mode==MODE_INSERT) {
    sprintf(buf, "INSERT into player_main ");
    for (i=0; i<NUM_PLAYER_MAIN_ROW_ELEMENTS; i++)
      sprintf(buf+strlen(buf), "%c%s", !i ? '(' : ',', querystring[i]);
    sprintf(buf+strlen(buf), ") VALUES(");
  }
  else if (mode==MODE_UPDATE)
    sprintf(buf, "UPDATE player_main SET ");

  init_querystring(ch, 1);

  for (i=0; i<NUM_PLAYER_MAIN_ROW_ELEMENTS; i++) {
    buf2[0]=0;
    if (!columnptrs[i].ptr || !TYPECAST(i, *columnptrs[i].ptr) || (columnptrs[i].type==1 && !*((char **)columnptrs[i].ptr)))
      sprintf(tmpstr, "%sNULL", mode==MODE_UPDATE ? mkqueryset(querystring[i]) : "," );
    else switch (columnptrs[i].type) {
      case 0: sprintf(tmpstr, "%s%ld",  mode==MODE_UPDATE ? mkqueryset(querystring[i]) : ",", TYPECAST(i, *columnptrs[i].ptr)); break;
      case 1:
        mysql_escape_string(buf2, *((char **)columnptrs[i].ptr), strlen(*((char **)columnptrs[i].ptr)));
        sprintf(tmpstr, "%s'%s'", mode==MODE_UPDATE ? mkqueryset(querystring[i]) : ",", buf2);
        break;
    }
/*    sprintf(buf1, "'%s' %d", tmpstr, i);
    log(buf1); */
    if (!fpass) { fpass=1; strcat(buf, tmpstr+1); }
    else strcat(buf, tmpstr);
  }
  if (mode==MODE_INSERT)
    strcat(buf, ")");
  else if (mode==MODE_UPDATE)
    sprintf(buf+strlen(buf), " WHERE name='%s'", GET_NAME(ch));

  QUERY_DATABASE(SQLdb, buf, strlen(buf));

  dbmodify_player_affects(ch, MODE_RETRIEVE);

  for (i = 0; i < NUM_WEARS; i++)
    {
      if (char_eq[i])
	nequip_char (ch, char_eq[i], i);
    }

  if (mysql_affected_rows(SQLdb) != 1 && mysql_errno(SQLdb)) {
    sprintf(buf, "ERROR: Could not INSERT/UPDATE playerfile properly (%d rows affected) for %s: %s", (int) mysql_affected_rows(SQLdb), GET_NAME(ch), mysql_error(SQLdb));
    log(buf);
    return 0;
  }
  return 1;
}

/* Retrieve a player entry into an existing character. */
struct char_data *retrieve_player_entry (char *name, struct char_data *ch) {
  int i;
  my_ulonglong rows;
  MYSQL_RES *result;
  MYSQL_ROW row;
  unsigned int fields;
  unsigned long *lengths;

  if (ch->player_specials == NULL)
    CREATE (ch->player_specials, struct player_special_data, 1);

  if (!querybuild) {
    init_querystring(ch, 0);
    querybuild=1;
  }

  if (!name || !*name) return NULL;
  sprintf(buf, "SELECT");
  for (i=0; i<NUM_PLAYER_MAIN_ROW_ELEMENTS; i++)
    sprintf(buf+strlen(buf), "%c%s", !i ? ' ' : ',', querystring[i]);
  sprintf(buf+strlen(buf), " FROM player_main WHERE name='%s'", name);
  QUERY_DATABASE(SQLdb, buf, strlen(buf));
  if (!(result=STORE_RESULT(SQLdb))) return 0;
  if ((rows=mysql_num_rows(result)) > 1 || rows<0) {
    sprintf(buf, "ERROR: SELECT for %s returned %d rows! Duplicate entries for seemingly VIABLE player!?", name, (int) rows);
    log(buf);
  }
  if (!rows) {
    mysql_free_result(result);
    return NULL;
  }
  if ((fields=mysql_num_fields(result)) != NUM_PLAYER_MAIN_ROW_ELEMENTS) { /* Someone changed the database and didn't modify the code to match, bitch them out and DO NOT DO ANY CONVERSIONS. */
    sprintf(buf, "WARNING: SELECT for %s returned %d columns per row! Code set to process %d columns!", name, fields, NUM_PLAYER_MAIN_ROW_ELEMENTS);
    log(buf);
    log(         "         MODIFY THE CODE IN DB.C AND DO IT CORRECTLY OR YOU WILL CORRUPT THE PLAYERFILE!");
    return NULL;
  }
  if (!(row=FETCH_ROW(result))) {
    mysql_free_result(result);
    return NULL;
  }

  lengths=mysql_fetch_lengths(result);

  init_querystring(ch, 1);
  
  for (i=0; i<NUM_PLAYER_MAIN_ROW_ELEMENTS; i++) {
    if (!lengths[i] && columnptrs[i].type!=-1) {
      if (columnptrs[i].type==0)
        setzero(columnptrs[i].ptr, structurevarsize[i]);
      else {
        *((char **)columnptrs[i].ptr) = (char *) malloc(1);
        **((char **)columnptrs[i].ptr) = 0;
      }
    }
    else {
      if (!columnptrs[i].type) {
        *columnptrs[i].ptr=ATOIROW(i);
      }
      else if (columnptrs[i].type==1) {
        if (*((char **)columnptrs[i].ptr)) free(*((char **)columnptrs[i].ptr));
        *((char **)columnptrs[i].ptr) = (char *) malloc(lengths[i]+1);
        memcpy(*((char **)columnptrs[i].ptr), row[i], lengths[i]);
        (*((char **)columnptrs[i].ptr))[lengths[i]] = 0;
      }
      else continue;
    }
  }

  memcpy(&ch->aff_abils, &ch->real_abils, sizeof(ch->real_abils));
  ch->player.short_descr = NULL;
  ch->player.long_descr = NULL;
  ch->player.time.logon = time (0);
  if (ch->points.max_mana < 100)
    ch->points.max_mana = 100;
  POOFIN (ch) = NULL;
  POOFOUT (ch) = NULL;
  ch->char_specials.carry_weight = 0;
  ch->char_specials.carry_items = 0;
  /*
   * If you're not poisioned and you've been away for more than an hour of
   * real time, we'll set your HMV back to full
   */

  if (!IS_AFFECTED (ch, AFF_POISON) &&
      (((long) (time (0) - ch->player.time.logon)) >= SECS_PER_REAL_HOUR))
    {
      GET_HIT (ch) = GET_MAX_HIT (ch);
      GET_MOVE (ch) = GET_MAX_MOVE (ch);
      GET_MANA (ch) = GET_MAX_MANA (ch);
    }
  mysql_free_result(result);
  dbmodify_player_affects(ch, MODE_RETRIEVE);
  dbmodify_player_skills(ch, MODE_RETRIEVE);
  return ch;
}

int count_player_entries ( void ) {
  my_ulonglong rows;
  MYSQL_RES *result;

  sprintf(buf, "SELECT idnum FROM player_main");
  QUERY_DATABASE(SQLdb, buf, strlen(buf));

  if (!(result=STORE_RESULT(SQLdb))) return 0;

  rows=mysql_num_rows(result);

  if (rows<0) { /* Wtf? */
    sprintf(buf, "ERROR: SELECT for all players returned %d rows: %s", (int) rows, mysql_error(SQLdb));
    log(buf);
  }

  mysql_free_result(result);
  return rows;
}

/* The first argument is the target name to load.
   The second argument is the type of each element in the third argument (either n for number or s for string).
   The third is a comma delimited list of column names you want to retrieve.
   The fourth argument and on are pointers you want to retrieve them into.
   eg.
   pe_printf("Storm", "nnsns", "idnum,level,title,race,description", GET_IDNUM(ch), GET_LEVEL(ch), GET_RACE(ch), GET_DESCRIPTION(ch));
*/
void pe_printf (char *name, char *types, char *querystr, ...) {
  va_list ap;
  int i=0, len;
  my_ulonglong rows;
  MYSQL_RES *result;
  MYSQL_ROW row;
  char buffer[32000];

  if (!name || !*name) return;

  sprintf(buffer, "SELECT %s from player_main WHERE name='%s'", querystr, name);
  QUERY_DATABASE(SQLdb, buffer, strlen(buffer));

  if (!(result=STORE_RESULT(SQLdb))) return;
  if ((rows=mysql_num_rows(result)) > 1 || rows<=0) {
    sprintf(buf, "ERROR: SELECT for %s returned %d rows! Duplicate or no player entries for seemingly VIABLE player!?", name, (int) rows);
    log(buf);
  }

  if (!(row=FETCH_ROW(result))) return;

  va_start(ap, querystr);
  len=strlen(types);
  while (i<len) {
    switch(types[i]) {
      case 'n':
        *(int *)va_arg(ap, int *)=ATOIROW(i); break;
      case 's':
        if (row[i]) strcpy(va_arg(ap, char *), row[i]);
        else strcpy(va_arg(ap, char *), "\0");
        break;
    }
    i++;
  }
  va_end(ap);
  mysql_free_result(result);
}
