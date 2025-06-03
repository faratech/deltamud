/* ************************************************************************
   *   File: db.c                                          Part of CircleMUD *
   *  Usage: Loading/saving chars, booting/resetting world, internal funcs   *
   *                                                                         *
   *  All rights reserved.  See license.doc for complete information.        *
   *                                                                         *
   *  Copyright (C) 1993, 94 by the Trustees of the Johns Hopkins University *
   *  CircleMUD is based on DikuMUD, Copyright (C) 1990, 1991.               *
   ************************************************************************ */

#define __DB_C__

#include "conf.h"
#include "sysdep.h"

#include "structs.h"
#include "utils.h"
#include "db.h"
#include "comm.h"
#include "handler.h"
#include "spells.h"
#include "mail.h"
#include "interpreter.h"
#include "house.h"
#include "auction.h"
#include "dg_scripts.h"
#include "gcmd.h"
#include "maputils.h"
#include "clan.h"
#include "dbinterface.h"

/**************************************************************************
*  declarations of most of the 'global' variables                         *
**************************************************************************/

struct room_data *world = NULL;	/* array of rooms                */
int top_of_world = 0;		/* ref to top element of world   */

struct char_data *character_list = NULL; /* global linked list of chars */

 struct index_data **trig_index; /* index table for triggers      */
 int top_of_trigt = 0;           /* top of trigger index table    */
 long max_id = 100000;           /* for unique mob/obj id's       */

struct index_data *mob_index;	/* index table for mobile file   */
struct char_data *mob_proto;	/* prototypes for mobs           */
int top_of_mobt = 0;		/* top of mobile index table     */

struct obj_data *object_list = NULL;	/* global linked list of objs    */
struct index_data *obj_index;	/* index table for object file   */
struct obj_data *obj_proto;	/* prototypes for objs           */
int top_of_objt = 0;		/* top of object index table     */

struct zone_data *zone_table;	/* zone table                    */
int top_of_zone_table = 0;	/* top element of zone tab       */
struct message_list fight_messages[MAX_MESSAGES];	/* fighting messages     */

struct player_index_element *player_table = NULL;	/* index to plr file     */
int top_of_p_table = 0;		/* ref to top of table           */
int top_of_p_file = 0;		/* ref of size of p file         */
long top_idnum = 0;		/* highest idnum in use          */

int no_mail = 0;		/* mail disabled?                */
int mini_mud = 0;		/* mini-mud mode?                */
int no_rent_check = 0;		/* skip rent check on boot?      */
time_t boot_time = 0;		/* time of mud boot              */
int circle_restrict = 0;		/* level of game restriction     */
/* long r_mortal_start_room;  */
long r_mortal_start_room[NUM_STARTROOMS + 1];
long r_immort_start_room;	/* rnum of immort start room     */
long r_frozen_start_room;	/* rnum of frozen start room     */

char *credits = NULL;		/* game credits                  */
char *news = NULL;		/* mud news                      */
char *motd = NULL;		/* message of the day - mortals */
char *imotd = NULL;		/* message of the day - immorts */
char *help = NULL;		/* help screen                   */
char *info = NULL;		/* info page                     */
char *circlemud = NULL;		/* circle credits                */
char *wizlist = NULL;		/* list of higher gods           */
char *immlist = NULL;		/* list of peon gods             */
char *background = NULL;	/* background story              */
char *handbook = NULL;		/* handbook for new immortals    */
char *policies = NULL;		/* policies page                 */
char *startup = NULL;		/* startup screen		 */

struct help_index_element *help_table = 0;	/* the help table        */
int top_of_helpt = 0;		/* top of help index table       */

struct time_info_data time_info;	/* the infomation about the time    */
struct weather_data weather_info;       /* the infomation about the weather */
struct player_special_data dummy_mob;	/* dummy spec area for mobs      */
struct reset_q_type reset_q;	/* queue of zones to be reset    */

/* local functions */
void clean_pfile(void);
void setup_dir (FILE * fl, int room, int dir);
void index_boot (int mode);
void discrete_load (FILE * fl, int mode);
void parse_trigger(FILE *fl, int virtual_nr);
void parse_room (FILE * fl, int virtual_nr);
void parse_mobile (FILE * mob_f, int nr);
char *parse_object (FILE * obj_f, int nr);
int t[6], j, k;
void load_zones (FILE * fl, char *zonename);
void load_help (FILE * fl);
void assign_mobiles (void);
void assign_objects (void);
void assign_rooms (void);
void assign_the_shopkeepers (void);
void build_player_index (void);
void boot_clans(void);
int is_empty (int zone_nr);
void reset_zone (int zone);
int file_to_string (char *name, char *buf);
int file_to_string_alloc (char *name, char **buf);
void check_start_rooms (void);
void renum_world (void);
void renum_zone_table (void);
void log_zone_error (int zone, int cmd_no, char *message);
void reset_time (void);
void clear_char (struct char_data *ch);
void read_mud_date_from_file(void);
void setup_special_dir(FILE * fl, int room);
void get_one_line(FILE *fl, char *buf);

/* external functions */
int can_edit_zone (struct char_data * ch, int number);
int real_zone (int number);
extern struct descriptor_data *descriptor_list;
extern struct auction_data auction;
void load_messages (void);
void mag_assign_spells (void);
void boot_social_messages (void);
void create_command_list(void);
void update_obj_file (void);	/* In objsave.c */
void sort_commands (void);
void sort_spells (void);
void load_banned (void);
void Read_Invalid_List (void);
void boot_the_shops (FILE * shop_f, char *filename, int rec_count);
SPECIAL (magic_user);

/* external vars */
extern int no_specials;

#define READ_SIZE 256

/*************************************************************************
*  routines for booting the system                                       *
*************************************************************************/

/* this is necessary for the autowiz system */
void
reboot_wizlists (void)
{
  file_to_string_alloc (WIZLIST_FILE, &wizlist);
  file_to_string_alloc (IMMLIST_FILE, &immlist);
}


ACMD (do_reboot)
{
  int i;

  one_argument (argument, arg);

  if (!str_cmp (arg, "all") || *arg == '*')
    {
      file_to_string_alloc (WIZLIST_FILE, &wizlist);
      file_to_string_alloc (IMMLIST_FILE, &immlist);
      file_to_string_alloc (NEWS_FILE, &news);
      file_to_string_alloc (CREDITS_FILE, &credits);
      file_to_string_alloc (CIRCLEMUD_FILE, &circlemud);
      file_to_string_alloc (MOTD_FILE, &motd);
      file_to_string_alloc (IMOTD_FILE, &imotd);
      file_to_string_alloc (HELP_PAGE_FILE, &help);
      file_to_string_alloc (INFO_FILE, &info);
      file_to_string_alloc (POLICIES_FILE, &policies);
      file_to_string_alloc (HANDBOOK_FILE, &handbook);
      file_to_string_alloc (BACKGROUND_FILE, &background);
      file_to_string_alloc (STARTUP_FILE, &startup);
    }
  else if (!str_cmp (arg, "wizlist"))
    file_to_string_alloc (WIZLIST_FILE, &wizlist);
  else if (!str_cmp (arg, "immlist"))
    file_to_string_alloc (IMMLIST_FILE, &immlist);
  else if (!str_cmp (arg, "news"))
    file_to_string_alloc (NEWS_FILE, &news);
  else if (!str_cmp (arg, "credits"))
    file_to_string_alloc (CREDITS_FILE, &credits);
  else if (!str_cmp (arg, "circlemud"))
    file_to_string_alloc (CIRCLEMUD_FILE, &circlemud);
  else if (!str_cmp (arg, "motd"))
    file_to_string_alloc (MOTD_FILE, &motd);
  else if (!str_cmp (arg, "imotd"))
    file_to_string_alloc (IMOTD_FILE, &imotd);
  else if (!str_cmp (arg, "help"))
    file_to_string_alloc (HELP_PAGE_FILE, &help);
  else if (!str_cmp (arg, "info"))
    file_to_string_alloc (INFO_FILE, &info);
  else if (!str_cmp (arg, "policy"))
    file_to_string_alloc (POLICIES_FILE, &policies);
  else if (!str_cmp (arg, "handbook"))
    file_to_string_alloc (HANDBOOK_FILE, &handbook);
  else if (!str_cmp (arg, "background"))
    file_to_string_alloc (BACKGROUND_FILE, &background);
  else if (!str_cmp(arg, "startup"))
    file_to_string_alloc(STARTUP_FILE, &startup);
  else if (!str_cmp(arg, "xhelp")) {
    if (help_table) {
      for (i = 0; i <= top_of_helpt; i++) {
        if (help_table[i].keywords)
          free(help_table[i].keywords);
        if (help_table[i].entry)
          free(help_table[i].entry);
      }
      free(help_table);
    }
    top_of_helpt = 0;   
    index_boot(DB_BOOT_HLP);
 } else
    {
      send_to_char ("Unknown reload option.\r\n", ch);
      return;
    }

  send_to_char (OK, ch);
}


void boot_world (void)
{
  log ("Loading zone table.");
  index_boot (DB_BOOT_ZON);

  log("Loading triggers and generating index.");
  index_boot(DB_BOOT_TRG);

  log ("Loading rooms.");
  index_boot (DB_BOOT_WLD);

  log ("Renumbering rooms.");
  renum_world ();

  log ("Checking start rooms.");
  check_start_rooms ();

  log ("Loading mobs and generating index.");
  index_boot (DB_BOOT_MOB);

  log ("Loading objs and generating index.");
  index_boot (DB_BOOT_OBJ);

  log ("Renumbering zone table.");
  renum_zone_table ();

  log ("Loading surface map.");
  read_map();

  if (!no_specials)
    {
      log ("Loading shops.");
      index_boot (DB_BOOT_SHP);
    }
}


/* body of the booting system */
void
boot_db (void)
{
  int i;
  void auction_reset ();
  void connect_database ();

  log ("Boot db -- BEGIN.");

  log ("Resetting the game time:");
  reset_time ();

  log ("Reading news, credits, help, bground, info & motds.");
  file_to_string_alloc (NEWS_FILE, &news);
  file_to_string_alloc (CREDITS_FILE, &credits);
  file_to_string_alloc (CIRCLEMUD_FILE, &circlemud);
  file_to_string_alloc (MOTD_FILE, &motd);
  file_to_string_alloc (IMOTD_FILE, &imotd);
  file_to_string_alloc (HELP_PAGE_FILE, &help);
  file_to_string_alloc (INFO_FILE, &info);
  file_to_string_alloc (WIZLIST_FILE, &wizlist);
  file_to_string_alloc (IMMLIST_FILE, &immlist);
  file_to_string_alloc (POLICIES_FILE, &policies);
  file_to_string_alloc (HANDBOOK_FILE, &handbook);
  file_to_string_alloc (BACKGROUND_FILE, &background);
  file_to_string_alloc (STARTUP_FILE, &startup);

  boot_world ();

  log ("Loading help entries.");
  index_boot (DB_BOOT_HLP);

  log ("Initializing connection to mySQL database server...");
  connect_database();  

  log ("Generating player index.");
  build_player_index ();

  log ("Loading fight messages.");
  load_messages ();

  log ("Loading social messages.");
  boot_social_messages ();
  create_command_list(); /* aedit patch -- M. Scott */

  log ("Assigning function pointers:");

  if (!no_specials)
    {
      log ("   Mobiles.");
      assign_mobiles ();
      log ("   Shopkeepers.");
      assign_the_shopkeepers ();
      log ("   Objects.");
      assign_objects ();
      log ("   Rooms.");
      assign_rooms ();
    }
  log ("   Spells.");
  mag_assign_spells ();

  log ("Assigning spell and skill levels.");
  init_spell_levels ();

  log ("Auction system reset.");
  auction_reset ();
  auction.auctioneer = NULL;

  log ("Sorting command list and spells.");
  sort_commands ();
  sort_spells ();

  log ("Booting mail system.");
  if (!scan_file ())
    {
      log ("    Mail boot failed -- Mail system disabled");
      no_mail = 1;
    }
  log ("Reading banned site and invalid-name list.");
  load_banned ();
  Read_Invalid_List ();

  if (!no_rent_check)
    {
      log ("Deleting timed-out crash and rent files:");
      update_obj_file ();
      log ("Done.");
    }
  for (i = 0; i <= top_of_zone_table; i++)
    {
      sprintf (buf2, "Resetting %s (rooms %d-%d).",
	       zone_table[i].name, (i ? (zone_table[i - 1].top + 1) : 0),
	       zone_table[i].top);
      log (buf2);
      reset_zone (i);
    }

  reset_q.head = reset_q.tail = NULL;

  if (!mini_mud)
    {
      log ("Booting houses.");
      House_boot ();
    }
  log ("Booting clans.");
  boot_clans();

  boot_time = time (0);

  log ("Boot db -- DONE.");
}


/* reset the time in the game from file */
void
reset_time (void)
{
//  long beginning_of_time = 650336715;
  long beginning_of_time = time(0)-(SECS_PER_MUD_YEAR*1000);
  struct time_info_data mud_time_passed (time_t t2, time_t t1);

  time_info = mud_time_passed (time (0), beginning_of_time);
  read_mud_date_from_file();

  if (time_info.hours <= 4)
    weather_info.sunlight = SUN_DARK;
  else if (time_info.hours == 5)
    weather_info.sunlight = SUN_RISE;
  else if (time_info.hours <= 20)
    weather_info.sunlight = SUN_LIGHT;
  else if (time_info.hours == 21)
    weather_info.sunlight = SUN_SET;
  else
    weather_info.sunlight = SUN_DARK;

  sprintf (buf, "   Current Gametime: %dH %dD %dM %dY.", time_info.hours,
	   time_info.day, time_info.month, (int) time_info.year);
  log (buf);
}

/* generate index table for the player file */


void
build_player_index (void)
{
  int nr = -1, i;
  long recs;
  MYSQL_RES *result;
  MYSQL_ROW row;

  mudlog("Loading players database.",NRM, LVL_IMMORT, TRUE);

  sprintf(buf, "SELECT idnum,name,level FROM player_main");
  QUERY_DATABASE(SQLdb, buf, strlen(buf));

  if (!(result=STORE_RESULT(SQLdb))) return;

  recs=mysql_num_rows(result);

  if (recs<0) { /* Wtf? */
    sprintf(buf, "ERROR: SELECT for all players returned %ld rows: %s", recs, mysql_error(SQLdb));
    log(buf);
  }

  if (recs)
    {
      sprintf (buf, "   %ld players in database.", recs);
      log (buf);
      CREATE (player_table, struct player_index_element, recs);
    }
  else
    {
      player_table = NULL;
      top_of_p_file = top_of_p_table = -1;
      mysql_free_result(result);
      return;
    }
  while ((row=FETCH_ROW(result))) {
    nr++;
    CREATE (player_table[nr].name, char, strlen (row[1]) + 1);
    for (i = 0; (*(player_table[nr].name + i) = LOWER (*(row[1] + i))); i++);
    player_table[nr].id = ATOIROW(0);
    player_table[nr].level = ATOIROW(2);
    top_idnum = MAX (top_idnum, player_table[nr].id);
  }
  mysql_free_result(result);

  top_of_p_file = top_of_p_table = nr;
}

/*
 * Thanks to Andrey (andrey@alex-ua.com) for this bit of code, although I
 * did add the 'goto' and changed some "while()" into "do { } while()".
 *      -gg 6/24/98 (technically 6/25/98, but I care not.)
 */
int count_alias_records(FILE *fl)
{
  char key[READ_SIZE], next_key[READ_SIZE];
  char line[READ_SIZE], *scan;
  int total_keywords = 0;
    
  /* get the first keyword line */
  get_one_line(fl, key);
  
  while (*key != '$') {
    /* skip the text */
    do {
      get_one_line(fl, line);
      if (feof(fl))
        goto ackeof;
    } while (*line != '#');
  
    /* now count keywords */  
    scan = key;
    do {
      scan = one_word(scan, next_key);
      ++total_keywords; 
    } while (*next_key);
  
    /* get next keyword line (or $) */
    get_one_line(fl, key);
      
    if (feof(fl))  
      goto ackeof;  
  }
  
  return total_keywords;
    
  /* No, they are not evil. -gg 6/24/98 */
ackeof:
  log("SYSERR: Unexpected end of help file.");
  exit(1);      /* Some day we hope to handle these things better... */
} 

/* function to count how many hash-mark delimited records exist in a file */
int
count_hash_records (FILE * fl)
{
  char buf[128];
  int count = 0;

  while (fgets (buf, 128, fl))
    if (*buf == '#')
      count++;

  return count;
}



void
index_boot (int mode)
{
  char *index_filename, *prefix;
  FILE *index, *db_file;
  int rec_count = 0;

  switch (mode)
    {
    case DB_BOOT_TRG:
      prefix = TRG_PREFIX;
      break;
    case DB_BOOT_WLD:
      prefix = WLD_PREFIX;
      break;
    case DB_BOOT_MOB:
      prefix = MOB_PREFIX;
      break;
    case DB_BOOT_OBJ:
      prefix = OBJ_PREFIX;
      break;
    case DB_BOOT_ZON:
      prefix = ZON_PREFIX;
      break;
    case DB_BOOT_SHP:
      prefix = SHP_PREFIX;
      break;
    case DB_BOOT_HLP:
      prefix = HLP_PREFIX;
      break;
    default:
      log ("SYSERR: Unknown subcommand to index_boot!");
      exit (1);
      break;
    }

  if (mini_mud)
    index_filename = MINDEX_FILE;
  else
    index_filename = INDEX_FILE;

  sprintf (buf2, "%s/%s", prefix, index_filename);

  if (!(index = fopen (buf2, "r")))
    {
      sprintf (buf1, "SYSERR: opening index file '%s'", buf2);
      perror (buf1);
      exit (1);
    }

  /* first, count the number of records in the file so we can malloc */
  fscanf (index, "%s\n", buf1);
  while (*buf1 != '$')
    {
      sprintf (buf2, "%s/%s", prefix, buf1);
      if (!(db_file = fopen (buf2, "r")))
	{
	  perror (buf2);
	  log ("SYSERR: file listed in index not found");
	  fscanf (index, "%s\n", buf1);
	  continue;
	}
      else
	{
	  if (mode == DB_BOOT_ZON)
	    rec_count++;
      else if (mode == DB_BOOT_HLP)
        rec_count += count_alias_records(db_file);
      else
        rec_count += count_hash_records(db_file);
	}

      fclose (db_file);
      fscanf (index, "%s\n", buf1);
    }

  /* Exit if 0 records, unless this is shops */
  if (!rec_count)
    {
      if (mode == DB_BOOT_SHP)
	return;
      log ("SYSERR: boot error - 0 records counted");
      exit (1);
    }

  rec_count++;

  switch (mode)
    {
   case DB_BOOT_TRG:
     CREATE(trig_index, struct index_data *, rec_count);
     break;
    case DB_BOOT_WLD:
      CREATE (world, struct room_data, rec_count);
      break;
    case DB_BOOT_MOB:
      CREATE (mob_proto, struct char_data, rec_count);
      CREATE (mob_index, struct index_data, rec_count);
      break;
    case DB_BOOT_OBJ:
      CREATE (obj_proto, struct obj_data, rec_count);
      CREATE (obj_index, struct index_data, rec_count);
      break;
    case DB_BOOT_ZON:
      CREATE (zone_table, struct zone_data, rec_count);
      break;
    case DB_BOOT_HLP:
      CREATE(help_table, struct help_index_element, rec_count);
      break;
    }

  rewind (index);
  fscanf (index, "%s\n", buf1);
  while (*buf1 != '$')
    {
      sprintf (buf2, "%s/%s", prefix, buf1);
      if (!(db_file = fopen (buf2, "r")))
	{
	  perror (buf2);
	  exit (1);
	}
      switch (mode)
	{
        case DB_BOOT_TRG:
	case DB_BOOT_WLD:
	case DB_BOOT_OBJ:
	case DB_BOOT_MOB:
	  discrete_load (db_file, mode);
	  break;
	case DB_BOOT_ZON:
	  load_zones (db_file, buf2);
	  break;
	case DB_BOOT_HLP:
	  load_help (db_file);
	  break;
	case DB_BOOT_SHP:
	  boot_the_shops (db_file, buf2, rec_count);
	  break;
	}

      fclose (db_file);
      fscanf (index, "%s\n", buf1);
    }

}


void
discrete_load (FILE * fl, int mode)
{
  int nr = -1, last = 0;
  char line[256];

  char *modes[] = {"world", "mob", "obj", "ZON", "SHP", "HLP", "trg"};
  /* modes positions correspond to DB_BOOT_xxx in db.h */

  for (;;)
    {
      /*
       * we have to do special processing with the obj files because they have
       * no end-of-record marker :(
       */
      if (mode != DB_BOOT_OBJ || nr < 0)
	if (!get_line (fl, line))
	  {
	    fprintf (logfile, "Format error after %s #%d\n", modes[mode], nr);
	    exit (1);
	  }
      if (*line == '$')
	return;

      if (*line == '#')
	{
	  last = nr;
	  if (sscanf (line, "#%d", &nr) != 1)
	    {
	      fprintf (logfile, "Format error after %s #%d\n", modes[mode], last);
	      exit (1);
	    }
	  if (nr >= MAX_ROOM_VNUM)
	    return;
	  else
	    switch (mode)
	      {
	      case DB_BOOT_TRG:
		parse_trigger(fl, nr);
		break;
	      case DB_BOOT_WLD:
		parse_room (fl, nr);
		break;
	      case DB_BOOT_MOB:
		parse_mobile (fl, nr);
		break;
	      case DB_BOOT_OBJ:
		strcpy (line, parse_object (fl, nr));
		break;
	      }
	}
      else
	{
	  fprintf (logfile, "Format error in %s file near %s #%d\n",
		   modes[mode], modes[mode], nr);
	  fprintf (logfile, "Offending line: '%s'\n", line);
	  exit (1);
	}
    }
}


long
asciiflag_conv (char *flag)
{
  long flags = 0;
  int is_number = 1;
  register char *p;

  for (p = flag; *p; p++)
    {
      if (islower (*p))
	flags |= 1 << (*p - 'a');
      else if (isupper (*p))
	flags |= 1 << (26 + (*p - 'A'));

      if (!isdigit (*p))
	is_number = 0;
    }

  if (is_number)
    flags = atol (flag);

  return flags;
}

char fread_letter(FILE *fp)
{
  char c;
  do {
    c = getc(fp);  
  } while (isspace(c));
  return c;
}

/* load the rooms */
void
parse_room (FILE * fl, int virtual_nr)
{
  static int room_nr = 0, zone = 0;
  int t[10], i;
  char line[256], flags[128];
  struct extra_descr_data *new_descr;
  char letter;

  sprintf (buf2, "room #%d", virtual_nr);

  if (virtual_nr <= (zone ? zone_table[zone - 1].top : -1))
    {
      fprintf (logfile, "Room #%d is below zone %d.\n", virtual_nr, zone);
      exit (1);
    }
  while (virtual_nr > zone_table[zone].top)
    if (++zone > top_of_zone_table)
      {
	fprintf (logfile, "Room %d is outside of any zone.\n", virtual_nr);
	exit (1);
      }
  world[room_nr].zone = zone;
  world[room_nr].number = virtual_nr;
  world[room_nr].name = fread_string (fl, buf2);
  world[room_nr].description = fread_string (fl, buf2);

  if (!get_line (fl, line) || sscanf (line, " %d %s %d ", t, flags, t + 2) != 3)
    {
      fprintf (logfile, "Format error in room #%d\n", virtual_nr);
      exit (1);
    }
  /* t[0] is the zone number; ignored with the zone-file system */
  world[room_nr].room_flags = asciiflag_conv (flags);
  world[room_nr].sector_type = t[2];

  world[room_nr].func = NULL;
  world[room_nr].contents = NULL;
  world[room_nr].people = NULL;
  world[room_nr].light = 0;	/* Zero light sources */

  for (i = 0; i < NUM_OF_DIRS; i++)
    world[room_nr].dir_option[i] = NULL;

  world[room_nr].ex_description = NULL;
  world[room_nr].special_exit = NULL;

  sprintf (buf, "SYSERR: Format error in room #%d (expecting D/E/S)", virtual_nr);

  for (;;)
    {
      if (!get_line (fl, line))
	{
	  fprintf (logfile, "Format error, %s\n", buf2);
	  exit (1);
	}
      switch (*line)
	{
        case 'O':
        setup_special_dir(fl, room_nr);
         break;
	case 'D':
	  setup_dir (fl, room_nr, atoi (line + 1));
	  break;
	case 'E':
	  CREATE (new_descr, struct extra_descr_data, 1);
	  new_descr->keyword = fread_string (fl, buf2);
	  new_descr->description = fread_string (fl, buf2);
	  new_descr->next = world[room_nr].ex_description;
	  world[room_nr].ex_description = new_descr;
	  break;
	case 'S':		/* end of room */
       /* DG triggers -- script is defined after the end of the room */
       letter = fread_letter(fl);
       ungetc(letter, fl);
       while (letter=='T') {
         dg_read_trigger(fl, &world[room_nr], WLD_TRIGGER);
         letter = fread_letter(fl);
         ungetc(letter, fl);
       }
	  top_of_world = room_nr++;
	  return;
	  break;
	default:
	  fprintf (logfile, "%s\n", buf);
	  exit (1);
	  break;
	}
    }
}



/* read direction data */
void
setup_dir (FILE * fl, int room, int dir)
{
  int t[5];
  char line[256];

  sprintf (buf2, "room #%d, direction D%d", (int) world[room].number, dir);

  CREATE (world[room].dir_option[dir], struct room_direction_data, 1);
  world[room].dir_option[dir]->general_description = fread_string (fl, buf2);
  world[room].dir_option[dir]->keyword = fread_string (fl, buf2);

  if (!get_line (fl, line))
    {
      fprintf (logfile, "Format error, %s\n", buf2);
      exit (1);
    }
  if (sscanf (line, " %d %d %d ", t, t + 1, t + 2) != 3)
    {
      fprintf (logfile, "Format error, %s\n", buf2);
      exit (1);
    }
  world[room].dir_option[dir]->exit_info = 0;
  if (t[0] > 2) {
    world[room].dir_option[dir]->exit_info = EX_HIDDEN;
    t[0]-=3;
  }
  if (t[0] == 1)
    world[room].dir_option[dir]->exit_info = world[room].dir_option[dir]->exit_info | EX_ISDOOR;
  else if (t[0] == 2)
    world[room].dir_option[dir]->exit_info = world[room].dir_option[dir]->exit_info | EX_ISDOOR
| EX_PICKPROOF;
  else
    world[room].dir_option[dir]->exit_info = world[room].dir_option[dir]->exit_info | 0;

  world[room].dir_option[dir]->key = t[1];
  world[room].dir_option[dir]->to_room = t[2];
}

/* read direction data */
void setup_special_dir(FILE * fl, int room)
{
  int t[5];
  char line[256];

//  sprintf(buf2, "room #%d, special exit.", GET_ROOM_VNUM(room));

  CREATE(world[room].special_exit, struct room_special_exit_data, 1);
  world[room].special_exit->general_description = fread_string(fl, buf2);
  world[room].special_exit->keyword = fread_string(fl, buf2);
  world[room].special_exit->ex_name = fread_string(fl, buf2);
  world[room].special_exit->leave_msg = fread_string(fl, buf2);

  if (!get_line(fl, line))
    exit(1);

  if (sscanf(line, "%d %d %d", t, t + 1, t + 2) != 3)
    exit(1);

  world[room].special_exit->exit_info = 0;
  if (t[0] > 2) {
    world[room].special_exit->exit_info = EX_HIDDEN;
    t[0]-=3;
  }
  if (t[0] == 1)
    world[room].special_exit->exit_info = world[room].special_exit->exit_info | EX_ISDOOR;
  else if (t[0] == 2)
    world[room].special_exit->exit_info = world[room].special_exit->exit_info | EX_ISDOOR |
EX_PICKPROOF;
  else
    world[room].special_exit->exit_info = world[room].special_exit->exit_info | 0;

  world[room].special_exit->key = t[1];
  world[room].special_exit->to_room = t[2];
}


/* make sure the start rooms exist & resolve their vnums to rnums */
void
check_start_rooms (void)
{
  int count;
  extern long mortal_start_room[NUM_STARTROOMS + 1]; 
  extern long immort_start_room;
/*  extern long frozen_start_room; */

  for (count = 1; count <= (NUM_STARTROOMS + 1); count++)
    if ((r_mortal_start_room[count] = real_room (mortal_start_room[count])) < 0)
      {
	if (count > 1)
	  r_mortal_start_room[count] = real_room (mortal_start_room[1]);
	else
	  {
	    log ("SYSERR:  Mortal start room does not exist.  Change in config.c.");
	    exit (1);
	  }
      }

  if ((r_immort_start_room = real_room (immort_start_room)) < 0)
    {
      if (!mini_mud)
	log ("SYSERR:  Warning: Immort start room does not exist.  Change in config.c.");
      r_frozen_start_room = r_mortal_start_room[1];
    }
}

/* resolve all vnums into rnums in the world */
void
renum_world (void)
{
  register int room, door;

  for (room = 0; room <= top_of_world; room++) {
    for (door = 0; door < NUM_OF_DIRS; door++)
      if (world[room].dir_option[door])
	if (world[room].dir_option[door]->to_room != NOWHERE)
	  world[room].dir_option[door]->to_room =
	    real_room (world[room].dir_option[door]->to_room);
     if (world[room].special_exit)
       world[room].special_exit->to_room = real_room(world[room].special_exit->to_room);
   }
}


#define ZCMD zone_table[zone].cmd[cmd_no]

/* resulve vnums into rnums in the zone reset tables */
void
renum_zone_table (void)
{
  int zone, cmd_no, a, b, c, olda, oldb, oldc;
  char buf[128];

  for (zone = 0; zone <= top_of_zone_table; zone++)
    for (cmd_no = 0; ZCMD.command != 'S'; cmd_no++)
      {
	a = b = c = 0;
	olda = ZCMD.arg1;
	oldb = ZCMD.arg2;
	oldc = ZCMD.arg3;
	switch (ZCMD.command)
	  {
	  case 'M':
	    a = ZCMD.arg1 = real_mobile (ZCMD.arg1);
	    c = ZCMD.arg3 = real_room (ZCMD.arg3);
	    break;
	  case 'O':
	    a = ZCMD.arg1 = real_object (ZCMD.arg1);
	    if (ZCMD.arg3 != NOWHERE)
	      c = ZCMD.arg3 = real_room (ZCMD.arg3);
	    break;
	  case 'G':
	    a = ZCMD.arg1 = real_object (ZCMD.arg1);
	    break;
	  case 'E':
	    a = ZCMD.arg1 = real_object (ZCMD.arg1);
	    break;
	  case 'P':
	    a = ZCMD.arg1 = real_object (ZCMD.arg1);
	    c = ZCMD.arg3 = real_object (ZCMD.arg3);
	    break;
	  case 'D':
	    a = ZCMD.arg1 = real_room (ZCMD.arg1);
	    break;
	  case 'R':		/* rem obj from room */
	    a = ZCMD.arg1 = real_room (ZCMD.arg1);
	    b = ZCMD.arg2 = real_object (ZCMD.arg2);
	    break;
	  }
	if (a < 0 || b < 0 || c < 0)
	  {
	    if (!mini_mud)
	      {
		sprintf (buf, "Invalid vnum %d, cmd disabled",
			 (a < 0) ? olda : ((b < 0) ? oldb : oldc));
		log_zone_error (zone, cmd_no, buf);
	      }
	    ZCMD.command = '*';
	  }
      }
}



void
parse_simple_mob (FILE * mob_f, int i, int nr)
{
  int j, t[12];
  char line[256];

  mob_proto[i].real_abils.str = MOB_DEFAULT_STAT;
  mob_proto[i].real_abils.intel = MOB_DEFAULT_STAT;
  mob_proto[i].real_abils.wis = MOB_DEFAULT_STAT;
  mob_proto[i].real_abils.dex = MOB_DEFAULT_STAT;
  mob_proto[i].real_abils.con = MOB_DEFAULT_STAT;
  mob_proto[i].real_abils.cha = MOB_DEFAULT_STAT;

  if (!get_line (mob_f, line))
    fprintf (logfile, "Format error in mob #%d, first line after S flag\n"
           "...expecting line of form 'X# # # # # # #d#+# #d#'\n", nr);
  if (line[0]!='X') { /* X = New modified version of reading the files (POWER/MPOWER/etc.)
                       here we're providing compatibility with old *and* new files. */
    if (sscanf (line, " %d %d %d %dd%d+%d %dd%d+%d ", t, t + 1, t + 2, t + 3, t + 4, t + 5, t + 6, t + 7, t + 8) != 9) {
      fprintf (logfile, "Format error in mob #%d, first line after S flag\n"
        "...expecting line of form ' # # # #d#+# #d#+#'\n", nr);
      exit (1);
    }
    GET_LEVEL (mob_proto + i) = t[0];
/*    mob_proto[i].points.hitroll = 20 - t[1];
    mob_proto[i].points.armor = 10 * t[2];
*/
    /* max hit = 0 is a flag that H, M, V is xdy+z */
    mob_proto[i].points.max_hit = 0;
    mob_proto[i].points.hit = t[3];
    mob_proto[i].points.mana = t[4];
    mob_proto[i].points.move = t[5];

    mob_proto[i].points.max_mana = 10;
    mob_proto[i].points.max_move = 50;

    mob_proto[i].mob_specials.damnodice = t[6];
    mob_proto[i].mob_specials.damsizedice = t[7];
/*    mob_proto[i].points.damroll = t[8]; */
  }
  else {
    if((sscanf (line, " X%d %d %d %d %d %d %dd%d+%d %dd%d ", t, t + 1, t + 2, t + 3, t + 4, t + 5, t + 6, t + 7, t + 8, t + 9, t + 10) != 11)) {
      fprintf (logfile, "Format error in mob #%d, first line after S flag\n"
        "...expecting line of form 'X# # # # # # #d#+# #d#'\n", nr);
      exit (1);
    }
    GET_LEVEL (mob_proto + i) = t[0];
    mob_proto[i].points.power = t[1];
    mob_proto[i].points.mpower = t[2];
    mob_proto[i].points.defense = t[3];
    mob_proto[i].points.mdefense = t[4];
    mob_proto[i].points.technique = t[5];

    /* max hit = 0 is a flag that H, M, V is xdy+z */
    mob_proto[i].points.max_hit = 0;
    mob_proto[i].points.hit = t[6];
    mob_proto[i].points.mana = t[7];
    mob_proto[i].points.move = t[8];

    mob_proto[i].points.max_mana = 10;
    mob_proto[i].points.max_move = 50;

    mob_proto[i].mob_specials.damnodice = t[9];
    mob_proto[i].mob_specials.damsizedice = t[10];
  }

  if (!get_line (mob_f, line) || (sscanf (line, " %d %d ", t, t + 1) != 2))
    {
      fprintf (logfile, "SYSERR: Format error in mob #%d, second line after S flag\n"
	       "...expecting line of form '# #'\n", nr);
      exit (1);
    }

  GET_GOLD (mob_proto + i) = t[0];
  GET_EXP (mob_proto + i) = t[1];

  if (!get_line (mob_f, line) || (sscanf (line, " %d %d %d ", t, t + 1, t + 2) != 3))
    {
      fprintf (logfile, "SYSERR: Format error in last line of mob #%d\n"
	       "...expecting line of form '# # #'\n", nr);
      exit (1);
    }

  mob_proto[i].char_specials.position = t[0];
  mob_proto[i].mob_specials.default_pos = t[1];

  /* TEMP fix to reassign all mob's positon since I added the POS_MEDITATING
     in between in the structs.h 
     Remove this code after a couple of reboots. 
     -Thargor-
     */

  if (mob_proto[i].char_specials.position >= POS_MEDITATING
      && mob_proto[i].char_specials.position < POS_STANDING)
    mob_proto[i].char_specials.position += 1;
  if (mob_proto[i].mob_specials.default_pos >= POS_MEDITATING
      && mob_proto[i].mob_specials.default_pos < POS_STANDING)
    mob_proto[i].mob_specials.default_pos += 1;

  /* END TEMP FIX*/

  mob_proto[i].player.sex = t[2];

  mob_proto[i].player.class = 0;
  mob_proto[i].player.weight = 200;
  mob_proto[i].player.height = 198;

  for (j = 0; j < 3; j++)
    GET_COND (mob_proto + i, j) = -100;
  
}


/*
 * interpret_espec is the function that takes espec keywords and values
 * and assigns the correct value to the mob as appropriate.  Adding new
 * e-specs is absurdly easy -- just add a new CASE statement to this
 * function!  No other changes need to be made anywhere in the code.
 */

#define CASE(test) if (!matched && !str_cmp(keyword, test) && (matched = 1))
#define RANGE(low, high) (num_arg = MAX((low), MIN((high), (num_arg))))

void
interpret_espec (char *keyword, char *value, int i, int nr)
{
  int num_arg, matched = 0;

  num_arg = atoi (value);

  CASE ("BareHandAttack")
  {
    RANGE (0, 99);
    mob_proto[i].mob_specials.attack_type = num_arg;
  }
  else if (num_arg==11) num_arg=MOB_DEFAULT_STAT;

  CASE ("Str")
  {
    RANGE (3, 25);
    mob_proto[i].real_abils.str = num_arg;
  }

  CASE ("StrAdd")
  {
    RANGE (0, 100);
    mob_proto[i].real_abils.str_add = num_arg;
  }

  CASE ("Int")
  {
    RANGE (3, 25);
    mob_proto[i].real_abils.intel = num_arg;
  }

  CASE ("Wis")
  {
    RANGE (3, 25);
    mob_proto[i].real_abils.wis = num_arg;
  }

  CASE ("Dex")
  {
    RANGE (3, 25);
    mob_proto[i].real_abils.dex = num_arg;
  }

  CASE ("Con")
  {
    RANGE (3, 25);
    mob_proto[i].real_abils.con = num_arg;
  }

  CASE ("Cha")
  {
    RANGE (3, 25);
    mob_proto[i].real_abils.cha = num_arg;
  }

  if (!matched)
    {
      fprintf (logfile, "SYSERR: Warning: unrecognized espec keyword %s in mob #%d\n",
	       keyword, nr);
    }
}

#undef CASE
#undef RANGE

void
parse_espec (char *buf, int i, int nr)
{
  char *ptr;

  if ((ptr = strchr (buf, ':')) != NULL)
    {
      *(ptr++) = '\0';
      while (isspace (*ptr))
	ptr++;
    }
  else
    ptr = "";

  interpret_espec (buf, ptr, i, nr);
}


void
parse_enhanced_mob (FILE * mob_f, int i, int nr)
{
  char line[256];

  parse_simple_mob (mob_f, i, nr);

  while (get_line (mob_f, line))
    {
      if (line[0]=='E')	/* end of the ehanced section */
	return;
      else if (*line == '#')
	{			/* we've hit the next mob, maybe? */
	  fprintf (logfile, "SYSERR: Unterminated E section in mob #%d\n", nr);
	  exit (1);
	}
      else
	parse_espec (line, i, nr);
    }

  fprintf (logfile, "SYESRR: Unexpected end of file reached after mob #%d\n", nr);
  exit (1);
}


void
parse_mobile (FILE * mob_f, int nr)
{
  static int i = 0;
  int j, t[10];
  char line[256], *tmpptr, letter;
  char f1[128], f2[128];

  mob_index[i].vnum = nr;
  mob_index[i].number = 0;
  mob_index[i].func = NULL;

  clear_char (mob_proto + i);

  mob_proto[i].player_specials = &dummy_mob;
  sprintf (buf2, "mob vnum %d", nr);

/***** String data *****/
  mob_proto[i].player.name = fread_string (mob_f, buf2);
  tmpptr = mob_proto[i].player.short_descr = fread_string (mob_f, buf2);
  if (tmpptr && *tmpptr)
    if (!str_cmp (fname (tmpptr), "a") || !str_cmp (fname (tmpptr), "an") ||
	!str_cmp (fname (tmpptr), "the"))
      *tmpptr = LOWER (*tmpptr);
  mob_proto[i].player.long_descr = fread_string (mob_f, buf2);
  mob_proto[i].player.description = fread_string (mob_f, buf2);
  mob_proto[i].player.title = NULL;

  /* *** Numeric data *** */
  if (!get_line (mob_f, line) || (sscanf (line, "%s %s %d %c", f1, f2, t + 2, &letter) != 4))
    {
      fprintf (logfile, "SYSERR: Format error after string section of mob #%d\n"
	       "...expecting line of form '# # # {S | E}'\n", nr);
      exit (1);
    }
  MOB_FLAGS (mob_proto + i) = asciiflag_conv (f1);
  SET_BIT (MOB_FLAGS (mob_proto + i), MOB_ISNPC);
  AFF_FLAGS (mob_proto + i) = asciiflag_conv (f2);
  GET_ALIGNMENT (mob_proto + i) = t[2];

  switch (UPPER (letter))
    {
    case 'S':			/* Simple monsters */
      parse_simple_mob (mob_f, i, nr);
      break;
    case 'E':			/* Circle3 Enhanced monsters */
      parse_enhanced_mob (mob_f, i, nr);
      break;
      /* add new mob types here.. */
    default:
      fprintf (logfile, "SYSERR: Unsupported mob type '%c' in mob #%d\n", letter, nr);
      exit (1);
      break;
    }

  /* DG triggers -- script info follows mob S/E section */
  letter = fread_letter(mob_f);
  ungetc(letter, mob_f);
  while (letter=='T') {
    dg_read_trigger(mob_f, &mob_proto[i], MOB_TRIGGER);
    letter = fread_letter(mob_f);
    ungetc(letter, mob_f);
  }

  mob_proto[i].aff_abils = mob_proto[i].real_abils;

  for (j = 0; j < NUM_WEARS; j++)
    mob_proto[i].equipment[j] = NULL;

  mob_proto[i].nr = i;
  mob_proto[i].desc = NULL;

//  if (MOB_FLAGGED(mob_index[i], MOB_CASTER))
  if (IS_SET(mob_proto[i].char_specials.saved.act, MOB_CASTER))
    mob_index[i].func = magic_user;

  top_of_mobt = i++;
}




/* read all objects from obj file; generate index and prototypes */
char *
parse_object (FILE * obj_f, int nr)
{
  static int i = 0, retval;
  static char line[256];
  int t[10], j;
  char *tmpptr;
  char f1[256], f2[256];
  struct extra_descr_data *new_descr;

  obj_index[i].vnum = nr;
  obj_index[i].number = 0;
  obj_index[i].func = NULL;

  clear_object (obj_proto + i);
  obj_proto[i].in_room = NOWHERE;
  obj_proto[i].item_number = i;

  sprintf (buf2, "object #%d", nr);

  /* *** string data *** */
  if ((obj_proto[i].name = fread_string (obj_f, buf2)) == NULL)
    {
      fprintf (logfile, "SYSERR: Null obj name or format error at or near %s\n", buf2);
      exit (1);
    }
  tmpptr = obj_proto[i].short_description = fread_string (obj_f, buf2);
  if (tmpptr && *tmpptr)
    if (!str_cmp (fname (tmpptr), "a") || !str_cmp (fname (tmpptr), "an") ||
	!str_cmp (fname (tmpptr), "the"))
      *tmpptr = LOWER (*tmpptr);

  tmpptr = obj_proto[i].description = fread_string (obj_f, buf2);
  if (tmpptr && *tmpptr)
    *tmpptr = UPPER (*tmpptr);
  obj_proto[i].action_description = fread_string (obj_f, buf2);

  /* *** numeric data *** */
  if (!get_line (obj_f, line) ||
      (retval = sscanf (line, " %d %s %s", t, f1, f2)) != 3)
    {
      fprintf (logfile, "SYSERR: Format error in first numeric line (expecting 3 args, got %d), %s\n", retval, buf2);
      exit (1);
    }
  obj_proto[i].obj_flags.type_flag = t[0];
  obj_proto[i].obj_flags.extra_flags = asciiflag_conv (f1);
  obj_proto[i].obj_flags.wear_flags = asciiflag_conv (f2);

 if (!get_line(obj_f, line) ||
  (retval = sscanf(line, "%d %d %d %d %d %d", t, t + 1, t
   + 2, t + 3, t + 4, t + 5)) > 6) { // umm, still trying here!
    fprintf(stderr, "Format error in second numeric line (expecting 4
    or 6 args, got %d), %s\n", retval, buf2); exit(1);
 }

  obj_proto[i].obj_flags.value[0] = t[0];
  obj_proto[i].obj_flags.value[1] = t[1];
  obj_proto[i].obj_flags.value[2] = t[2];
  obj_proto[i].obj_flags.value[3] = t[3];
 if (t[4] < 0 || t[4] > 100) {
  obj_proto[i].obj_flags.curr_slots = 0;
  obj_proto[i].obj_flags.total_slots = 0;
 }
  else {
   obj_proto[i].obj_flags.curr_slots = t[4];
   obj_proto[i].obj_flags.total_slots = t[5];
 }
 if (t[5] < 0 || t[5] > 100) {
  obj_proto[i].obj_flags.curr_slots = 0;
  obj_proto[i].obj_flags.total_slots = 0;
 }
 else {
  obj_proto[i].obj_flags.curr_slots = t[4];
  obj_proto[i].obj_flags.total_slots = t[5];
 }


  if (!get_line (obj_f, line) ||
      (retval = sscanf (line, "%d %d %d", t, t + 1, t + 2)) != 3)
    {
      fprintf (logfile, "SYSERR: Format error in third numeric line (expecting 3 args, got %d), %s\n", retval, buf2);
      exit (1);
    }
  obj_proto[i].obj_flags.weight = t[0];
  obj_proto[i].obj_flags.cost = t[1];
  obj_proto[i].obj_flags.cost_per_day = t[2];

  /* check to make sure that weight of containers exceeds curr. quantity */
  if (obj_proto[i].obj_flags.type_flag == ITEM_DRINKCON ||
      obj_proto[i].obj_flags.type_flag == ITEM_FOUNTAIN)
    {
      if (obj_proto[i].obj_flags.weight < obj_proto[i].obj_flags.value[1])
	obj_proto[i].obj_flags.weight = obj_proto[i].obj_flags.value[1] + 5;
    }

  /* *** extra descriptions and affect fields *** */

  for (j = 0; j < MAX_OBJ_AFFECT; j++)
    {
      obj_proto[i].affected[j].location = APPLY_NONE;
      obj_proto[i].affected[j].modifier = 0;
    }

  obj_proto[i].obj_class=-1;
  obj_proto[i].min_level=0;

  strcat (buf2, ", after numeric constants\n...expecting 'E', 'A', '$', or next object number");
  j = 0;

  for (;;)
    {
      if (!get_line (obj_f, line))
	{
	  fprintf (logfile, "SYSERR: Format error in %s\n", buf2);
	  exit (1);
	}
      switch (*line)
	{
	case 'E':
	  CREATE (new_descr, struct extra_descr_data, 1);
	  new_descr->keyword = fread_string (obj_f, buf2);
	  new_descr->description = fread_string (obj_f, buf2);
	  new_descr->next = obj_proto[i].ex_description;
	  obj_proto[i].ex_description = new_descr;
	  break;
	case 'A':
	  if (j >= MAX_OBJ_AFFECT)
	    {
	      fprintf (logfile, "SYSERR: Too many A fields (%d max), %s\n", MAX_OBJ_AFFECT, buf2);
	      exit (1);
	    }
	  if (!get_line (obj_f, line) || (sscanf (line, " %d %d ", t, t + 1) != 2))
	    {
	      fprintf (logfile, "SYSERR: Format error in 'A' field, %s\n", buf2);
	      fprintf (logfile, "...offending line: '%s'\n", line);
	    }
	  obj_proto[i].affected[j].location = t[0];
	  obj_proto[i].affected[j].modifier = t[1];
	  j++;
	  break;
   //      case 'C':
  //    get_line(obj_f, line);
  //    sscanf(line, "%d ", t);
   //   obj_proto[i].obj_flags.bitvector = obj_proto[i].obj_flags.bitvector | t[0];
 //     break;
       case 'c':
         obj_proto[i].obj_class=atoi(line+2)-1;
         break;
       case 'L':
         obj_proto[i].min_level=atoi(line+2);
         break;
       case 'B':
         if (*(line+1)=='V')
           obj_proto[i].obj_flags.bitvector=atoi(line+3);
         break;
       case 'T':  /* DG triggers */
         dg_obj_trigger(line, &obj_proto[i]);
         break;
	case '$':
	case '#':
	  top_of_objt = i++;
	  return line;
	  break;
	default:
	  fprintf (logfile, "SYSERR: Format error in %s\n", buf2);
	  exit (1);
	  break;
	}
    }
}


#define Z	zone_table[zone]

/* load the zone table and command tables */
void
load_zones (FILE * fl, char *zonename)
{
  static int zone = 0;
  int cmd_no = 0, num_of_cmds = 0, line_num = 0, tmp, error;
  char *ptr, buf[256], zname[256];

  strcpy (zname, zonename);

  while (get_line (fl, buf))
    num_of_cmds++;		/* this should be correct within 3 or so */
  rewind (fl);

  if (num_of_cmds == 0)
    {
      fprintf (logfile, "SYSERR: %s is empty!\n", zname);
      exit (0);
    }
  else
    CREATE (Z.cmd, struct reset_com, num_of_cmds);

  line_num += get_line (fl, buf);

  if (sscanf (buf, "#%d", &Z.number) != 1)
    {
      fprintf (logfile, "SYSERR: Format error in %s, line %d\n", zname, line_num);
      exit (0);
    }
  sprintf (buf2, "beginning of zone #%d", Z.number);

  line_num += get_line (fl, buf);

  if ((ptr = strchr (buf, '~')) != NULL)	/* take off the '~' if it's there */
    *ptr = '\0';
  Z.name = str_dup (buf);

  line_num += get_line(fl, buf); 

  if ((ptr = strchr(buf, '~')) != NULL) /* take off the '~' if it's there */
    *ptr = '\0';
  Z.builders = str_dup(buf);

  line_num += get_line (fl, buf);

  if (sscanf (buf, "%d %d %d ", &Z.top, &Z.lifespan, &Z.reset_mode) != 3)
    {
      fprintf (logfile, "SYSERR: Format error in 3-constant line of %s", zname);
      exit (1);
    }
 
   line_num += get_line(fl, buf);
   if (sscanf(buf, "%d %d %d ", &Z.lvl1, &Z.lvl2, &Z.status_mode) != 3)
{
     fprintf(stderr, "Format error in 3-constant line of %s", zname);
     exit(0);
   }

  cmd_no = 0;

  for (;;)
    {
      if ((tmp = get_line (fl, buf)) == 0)
	{
	  fprintf (logfile, "Format error in %s - premature end of file\n", zname);
	  exit (1);
	}
      line_num += tmp;
      ptr = buf;
      skip_spaces (&ptr);

      if ((ZCMD.command = *ptr) == '*')
	continue;

      ptr++;

      if (ZCMD.command == 'S' || ZCMD.command == '$')
	{
	  ZCMD.command = 'S';
	  break;
	}
      error = 0;
/*      if (strchr ("MOEPD", ZCMD.command) == NULL)
	{			 a 3-arg command
	  if (sscanf (ptr, " %d %d %d ", &tmp, &ZCMD.arg1, &ZCMD.arg2) != 3)
	    error = 1;
	}
      else
	{ */
	  if (sscanf (ptr, " %d %d %d %d %d", &tmp, &ZCMD.arg1, &ZCMD.arg2,
		      &ZCMD.arg3, &ZCMD.arg4) != 5)
	    error = 1;
	// }

      ZCMD.if_flag = tmp;

      if (error)
	{
	  fprintf (logfile, "SYSERR: Format error in %s, line %d: '%s'\n", zname, line_num, buf);
	  exit (1);
	}
      ZCMD.line = line_num;
      cmd_no++;
    }

  top_of_zone_table = zone++;
}

#undef Z


void
get_one_line (FILE * fl, char *buf)
{
  if (fgets (buf, READ_SIZE, fl) == NULL)
    {
      log ("SYSERR: error reading help file: not terminated with $?");
      exit (1);
    }

  buf[strlen (buf) - 1] = '\0';	/* take off the trailing \n */
}


void load_help(FILE *fl)
{
  char key[READ_SIZE+1], entry[32384];
  char line[READ_SIZE+1];
  struct help_index_element el;

  /* get the keyword line */
  get_one_line(fl, key);
  while (*key != '$') {
    get_one_line(fl, line);
    *entry = '\0';
    while (*line != '#') {
      strcat(entry, strcat(line, "\r\n"));
      get_one_line(fl, line);
    }
 
    el.min_level = 0;
    if ((*line == '#') && (*(line + 1) != 0))
      el.min_level = atoi((line + 1));

    el.min_level = MAX(0, MIN(el.min_level, LVL_IMPL));

    /* now, add the entry to the index with each keyword on the keyword line */
    el.entry = str_dup(entry);
    el.keywords = str_dup(key);
    
    help_table[top_of_helpt] = el;
    top_of_helpt++;
     
    /* get next keyword line (or $) */
    get_one_line(fl, key);
  }
}

//int
//hsort (const void *a, const void *b)
//{
//  struct help_index_element *a1, *b1;
//
//  a1 = (struct help_index_element *) a;
//  b1 = (struct help_index_element *) b;
//
//  return (str_cmp (a1->keyword, b1->keyword));
//}


/*************************************************************************
*  procedures for resetting, both play-time and boot-time	 	 *
*************************************************************************/



int
vnum_mobile (char *searchname, struct char_data *ch)
{
  int nr, found = 0;

  for (nr = 0; nr <= top_of_mobt; nr++)
    {
      if (isname (searchname, mob_proto[nr].player.name) && (can_edit_zone(ch,
real_zone(mob_index[nr].vnum)) || GET_LEVEL(ch)>=LVL_IMMORT))
	{
	  sprintf (buf, "%3d. [%5d] %s\r\n", ++found,
		   mob_index[nr].vnum,
		   mob_proto[nr].player.short_descr);
	  send_to_char (buf, ch);
	}
    }

  return (found);
}



int
vnum_object (char *searchname, struct char_data *ch)
{
  int nr, found = 0;

  for (nr = 0; nr <= top_of_objt; nr++)
    {
      if (isname (searchname, obj_proto[nr].name) && (can_edit_zone(ch, 
real_zone(obj_index[nr].vnum)) || GET_LEVEL(ch)>=LVL_IMMORT))
	{
	  sprintf (buf, "%3d. [%5d] %s\r\n", ++found,
		   obj_index[nr].vnum,
		   obj_proto[nr].short_description);
	  send_to_char (buf, ch);
	}
    }
  return (found);
}


/* create a character, and add it to the char list */
struct char_data *
create_char (void)
{
  struct char_data *ch;

  CREATE (ch, struct char_data, 1);
  clear_char (ch);
  ch->next = character_list;
  character_list = ch;

  GET_ID(ch) = max_id++;
  return ch;
}


/* create a new mobile from a prototype */
struct char_data *
read_mobile (int nr, int type)
{
  int i;
  struct char_data *mob;

  if (type == VIRTUAL)
    {
      if ((i = real_mobile (nr)) < 0)
	{
	  sprintf (buf, "Mobile (V) %d does not exist in database.", nr);
	  log (buf);
	  return NULL;
	}
    }
  else
    i = nr;

  CREATE (mob, struct char_data, 1);
  clear_char (mob);
  *mob = mob_proto[i];
  mob->next = character_list;
  character_list = mob;

  if (!mob->points.max_hit)
    {
      mob->points.max_hit = dice (mob->points.hit, mob->points.mana) +
	mob->points.move;
    }
  else
    mob->points.max_hit = number (mob->points.hit, mob->points.mana);

  mob->points.hit = mob->points.max_hit;
  mob->points.mana = mob->points.max_mana;
  mob->points.move = mob->points.max_move;

  mob->player.time.birth = time (0);
  mob->player.time.played = 0;
  mob->player.time.logon = time (0);

  mob_index[i].number++;
  GET_ID(mob) = max_id++;
  assign_triggers(mob, MOB_TRIGGER);


  return mob;
}


/* create an object, and add it to the object list */
struct obj_data *
create_obj (void)
{
  struct obj_data *obj;

  CREATE (obj, struct obj_data, 1);
  clear_object (obj);
  obj->next = object_list;
  object_list = obj;

  GET_ID(obj) = max_id++;
  assign_triggers(obj, OBJ_TRIGGER);

  return obj;
}


/* create a new object from a prototype */
struct obj_data *
read_object (int nr, int type)
{
  struct obj_data *obj;
  int i;

  if (nr < 0)
    {
      log ("SYSERR: trying to create obj with negative num!");
      return NULL;
    }
  if (type == VIRTUAL)
    {
      if ((i = real_object (nr)) < 0)
	{
	  sprintf (buf, "Object (V) %d does not exist in database.", nr);
	  log (buf);
	  return NULL;
	}
    }
  else
    i = nr;

  CREATE (obj, struct obj_data, 1);
  clear_object (obj);
  *obj = obj_proto[i];
  obj->next = object_list;
  object_list = obj;

  obj_index[i].number++;

  GET_ID(obj) = max_id++;
  assign_triggers(obj, OBJ_TRIGGER);

  return obj;
}



#define ZO_DEAD  999

/* update zone ages, queue for reset if necessary, and dequeue when possible */
void
zone_update (void)
{
  int i;
  struct reset_q_element *update_u, *temp;
  static int timer = 0;
  char buf[128];

  /* jelson 10/22/92 */
  if (((++timer * PULSE_ZONE) / PASSES_PER_SEC) >= 60)
    {
      /* one minute has passed */
      /*
       * NOT accurate unless PULSE_ZONE is a multiple of PASSES_PER_SEC or a
       * factor of 60
       */

      timer = 0;

      /* since one minute has passed, increment zone ages */
      for (i = 0; i <= top_of_zone_table; i++)
	{
	  if (zone_table[i].age < zone_table[i].lifespan &&
	      zone_table[i].reset_mode)
	    (zone_table[i].age)++;

	  if (zone_table[i].age >= zone_table[i].lifespan &&
	      zone_table[i].age < ZO_DEAD && zone_table[i].reset_mode)
	    {
	      /* enqueue zone */

	      CREATE (update_u, struct reset_q_element, 1);

	      update_u->zone_to_reset = i;
	      update_u->next = 0;

	      if (!reset_q.head)
		reset_q.head = reset_q.tail = update_u;
	      else
		{
		  reset_q.tail->next = update_u;
		  reset_q.tail = update_u;
		}

	      zone_table[i].age = ZO_DEAD;
	    }
	}
    }				/* end - one minute has passed */


  /* dequeue zones (if possible) and reset */
  /* this code is executed every 10 seconds (i.e. PULSE_ZONE) */
  for (update_u = reset_q.head; update_u; update_u = update_u->next)
    if (zone_table[update_u->zone_to_reset].reset_mode == 2 ||
	is_empty (update_u->zone_to_reset))
      {
	reset_zone (update_u->zone_to_reset);
	sprintf (buf, "Auto zone reset: %s",
		 zone_table[update_u->zone_to_reset].name);
	mudlog (buf, CMP, LVL_GOD, FALSE);
	/* dequeue */
	if (update_u == reset_q.head)
	  reset_q.head = reset_q.head->next;
	else
	  {
	    for (temp = reset_q.head; temp->next != update_u;
		 temp = temp->next);

	    if (!update_u->next)
	      reset_q.tail = temp;

	    temp->next = update_u->next;
	  }

	free (update_u);
	break;
      }
}

void
log_zone_error (int zone, int cmd_no, char *message)
{
  char buf[256];

  sprintf (buf, "SYSERR: zone file: %s", message);
  mudlog (buf, NRM, LVL_GOD, TRUE);

  sprintf (buf, "SYSERR: ...offending cmd: '%c' cmd in zone #%d, line %d",
	   ZCMD.command, zone_table[zone].number, ZCMD.line);
  mudlog (buf, NRM, LVL_GOD, TRUE);
}

#define ZONE_ERROR(message) \
	{ log_zone_error(zone, cmd_no, message); last_cmd = 0; }

/* execute the reset command table of a given zone */
void
reset_zone (int zone)
{
  int cmd_no, last_cmd = 0;
  struct char_data *mob = NULL;
  struct obj_data *obj=NULL, *obj_to;
  int room_vnum, room_rnum;
  int mob_load = FALSE; /* ### */
  int obj_load = FALSE; /* ### */

  for (cmd_no = 0; ZCMD.command != 'S'; cmd_no++)
    {

    if (ZCMD.if_flag && !last_cmd && !mob_load && !obj_load)
      continue;

    if (!ZCMD.if_flag) { /* ### */
      mob_load = FALSE;
      obj_load = FALSE;
    }

      switch (ZCMD.command)
	{
	case '*':		/* ignore command */
	  last_cmd = 0;
	  break;

	case 'M':		/* read a mobile */
	  if (mob_index[ZCMD.arg1].number < ZCMD.arg2 && (number(1, 100) >= ZCMD.arg4))
	    {
	      mob = read_mobile (ZCMD.arg1, REAL);
	      char_to_room (mob, ZCMD.arg3);
              load_mtrigger(mob);
	      last_cmd = 1;
              mob_load = TRUE;
	    }
	  else
	    last_cmd = 0;
	  break;

	case 'O':		/* read an object */
	  if (obj_index[ZCMD.arg1].number < ZCMD.arg2 && (number(1, 100) >= ZCMD.arg4))
	    {
	      if (ZCMD.arg3 >= 0)
		{
		  obj = read_object (ZCMD.arg1, REAL);
		  obj_to_room (obj, ZCMD.arg3);
                  load_otrigger(obj);
		  last_cmd = 1;
                  obj_load = TRUE;
		}
	      else
		{
		  obj = read_object (ZCMD.arg1, REAL);
		  obj->in_room = NOWHERE;
		  last_cmd = 1;
                  obj_load = TRUE;
		}
	    }
	  else
	    last_cmd = 0;
	  break;

    case 'P':                   /* object to object */
      if ((obj_index[ZCMD.arg1].number < ZCMD.arg2) &&
          obj_load && (number(1, 100) >= ZCMD.arg4)) {
        obj = read_object(ZCMD.arg1, REAL);
        if (!(obj_to = get_obj_num(ZCMD.arg3))) {
          ZONE_ERROR("target obj not found");
          break;
        }
        obj_to_obj(obj, obj_to);
        load_otrigger(obj);
        last_cmd = 1;
      } else
        last_cmd = 0;
      break;

    case 'G':                   /* obj_to_char ### */ 
      if (!mob) {
        ZONE_ERROR("attempt to give obj to non-existant mob");
        break;
      }
      if ((obj_index[ZCMD.arg1].number < ZCMD.arg2) &&
          mob_load && (number(1, 100) >= ZCMD.arg3)) {
        obj = read_object(ZCMD.arg1, REAL);
        obj_to_char(obj, mob);
        load_otrigger(obj);
        last_cmd = 1;
      } else
        last_cmd = 0;
      break;

    case 'E':                   /* object to equipment list ### */
      if (!mob) {
        ZONE_ERROR("trying to equip non-existant mob");
        break;
      }
      if ((obj_index[ZCMD.arg1].number < ZCMD.arg2) &&
          mob_load && (number(1, 100) >= ZCMD.arg4)) {
        if (ZCMD.arg3 < 0 || ZCMD.arg3 >= NUM_WEARS) {
          ZONE_ERROR("invalid equipment pos number");
        } else {
          obj = read_object(ZCMD.arg1, REAL);
          equip_char(mob, obj, ZCMD.arg3);
          load_otrigger(obj);
          last_cmd = 1;
        }
      } else
        last_cmd = 0;
      break;

	case 'R':		/* rem obj from room */
	  if ((obj = get_obj_in_list_num (ZCMD.arg2, world[ZCMD.arg1].contents)) != NULL)
	    {
	      obj_from_room (obj);
	      extract_obj (obj);
              obj=NULL;
	    }
	  last_cmd = 1;
	  break;


	case 'D':		/* set state of door */
	  if (ZCMD.arg2 < 0 || ZCMD.arg2 >= NUM_OF_DIRS ||
	      (world[ZCMD.arg1].dir_option[ZCMD.arg2] == NULL))
	    {
	      sprintf(buf, "door does not exist in room %d", (int) GET_ROOM_VNUM(ZCMD.arg1));
              ZONE_ERROR(buf);
	    }
	  else
	    switch (ZCMD.arg3)
	      {
	      case 0:
		REMOVE_BIT (world[ZCMD.arg1].dir_option[ZCMD.arg2]->exit_info,
			    EX_LOCKED);
		REMOVE_BIT (world[ZCMD.arg1].dir_option[ZCMD.arg2]->exit_info,
			    EX_CLOSED);
		break;
	      case 1:
		SET_BIT (world[ZCMD.arg1].dir_option[ZCMD.arg2]->exit_info,
			 EX_CLOSED);
		REMOVE_BIT (world[ZCMD.arg1].dir_option[ZCMD.arg2]->exit_info,
			    EX_LOCKED);
		break;
	      case 2:
		SET_BIT (world[ZCMD.arg1].dir_option[ZCMD.arg2]->exit_info,
			 EX_LOCKED);
		SET_BIT (world[ZCMD.arg1].dir_option[ZCMD.arg2]->exit_info,
			 EX_CLOSED);
		break;
	      }
	  last_cmd = 1;
	  break;

	default:
	  ZONE_ERROR ("unknown cmd in reset table; cmd disabled");
	  ZCMD.command = '*';
	  break;
	}
       if (!zone_table[zone].status_mode && obj)
         SET_BIT(obj->obj_flags.extra_flags, ITEM_NORENT);
    }

  zone_table[zone].age = 0;
  /* handle reset_wtrigger's */
  room_vnum = zone_table[zone].number * 100;
  while (room_vnum <= zone_table[zone].top) {
    room_rnum = real_room(room_vnum);
    if (room_rnum != NOWHERE) reset_wtrigger(&world[room_rnum]);
    room_vnum++;
  }
}



/* for use in reset_zone; return TRUE if zone 'nr' is free of PC's  */
int
is_empty (int zone_nr)
{
  struct descriptor_data *i;

  for (i = descriptor_list; i; i = i->next)
    if (!i->connected)
      if (world[i->character->in_room].zone == zone_nr)
	return 0;

  return 1;
}





/*************************************************************************
*  stuff related to the save/load player system				 *
*************************************************************************/


long
get_id_by_name (char *name)
{
  int i;

  one_argument (name, arg);
  for (i = 0; i <= top_of_p_table; i++)
    if (!strcmp ((player_table + i)->name, arg))
      return ((player_table + i)->id);

  return -1;
}


char *
get_name_by_id (long id)
{
  int i;

  for (i = 0; i <= top_of_p_table; i++)
    if ((player_table + i)->id == id)
      return ((player_table + i)->name);

  return NULL;
}

/*
 * write the vital data of a player to the player file
 *
 * NOTE: load_room should be an *RNUM* now.  It is converted to a vnum here.
 */
void
save_char (struct char_data *ch, long load_room)
{
  if (IS_NPC (ch) || !ch->desc)
    return;

  if (load_room == NOWHERE)
    ch->player_specials->saved.load_room = NOWHERE;
  else
    ch->player_specials->saved.load_room = world[load_room].number;
  insert_player_entry(ch); 
}

/* We won't ever need this, but just in case..
void store_to_char (struct char_file_u *st, struct char_data *ch)
{
  int i;

  if (ch->player_specials == NULL)
    CREATE (ch->player_specials, struct player_special_data, 1);

  GET_SEX (ch) = st->sex;
  GET_CLASS (ch) = st->class;
  GET_RACE (ch) = st->race;
  GET_LEVEL (ch) = st->level;
  GET_DEITY (ch) = st->deity;

  ch->player.short_descr = NULL;
  ch->player.long_descr = NULL;
  ch->player.title = str_dup (st->title);
  ch->player.description = str_dup (st->description);

  ch->player.hometown = st->hometown;
  ch->player.time.birth = st->birth;
  ch->player.time.played = st->played;
  ch->player.time.logon = time (0);

  ch->player.weight = st->weight;
  ch->player.height = st->height;

  ch->real_abils = st->abilities;
  ch->aff_abils = st->abilities;
  ch->points = st->points;
  ch->char_specials.saved = st->char_specials_saved;
  ch->player_specials->saved = st->player_specials_saved;
  POOFIN (ch) = NULL;
  POOFOUT (ch) = NULL;

  if (ch->points.max_mana < 100)
    ch->points.max_mana = 100;

  ch->char_specials.carry_weight = 0;
  ch->char_specials.carry_items = 0;

  if (ch->player.name == NULL)
    CREATE (ch->player.name, char, strlen (st->name) + 1);
  strcpy (ch->player.name, st->name);
  if (ch->player.passwd == NULL)
    CREATE (ch->player.passwd, char, strlen(st->pwd) + 1);
  strcpy (ch->player.passwd, st->pwd);

  for (i = 0; i < MAX_AFFECT; i++)
    {
      if (st->affected[i].type)
	affect_to_char (ch, &st->affected[i]);
    }

  if (!IS_AFFECTED (ch, AFF_POISON) &&
      (((long) (time (0) - st->last_logon)) >= SECS_PER_REAL_HOUR))
    {
      GET_HIT (ch) = GET_MAX_HIT (ch);
      GET_MOVE (ch) = GET_MAX_MOVE (ch);
      GET_MANA (ch) = GET_MAX_MANA (ch);
    }
}*/

void
save_etext (struct char_data *ch)
{
/* this will be really cool soon */

}


/* create a new entry in the in-memory index table for the player file */
int
create_entry (char *name)
{
  int i;

  if (top_of_p_table == -1)
    {
      CREATE (player_table, struct player_index_element, 1);
      top_of_p_table = 0;
    }
  else if (!(player_table = (struct player_index_element *)
	     realloc (player_table, sizeof (struct player_index_element) *
		        (++top_of_p_table + 1))))
    {
      perror ("SYSERR: create entry");
      exit (1);
    }
  CREATE (player_table[top_of_p_table].name, char, strlen (name) + 1);

  /* copy lowercase equivalent of name to table field */
  for (i = 0; (*(player_table[top_of_p_table].name + i) = LOWER (*(name + i)));
       i++);

  return (top_of_p_table);
}



/************************************************************************
*  funcs of a (more or less) general utility nature			*
************************************************************************/


/* read and allocate space for a '~'-terminated string from a given file */
char *
fread_string (FILE * fl, char *error)
{
  char buf[MAX_STRING_LENGTH], tmp[512], *rslt;
  register char *point;
  int done = 0, length = 0, templength = 0;

  *buf = '\0';

  do
    {
      if (!fgets (tmp, 512, fl))
	{
	  fprintf (logfile, "SYSERR: fread_string: format error at or near %s\n",
		   error);
	  exit (1);
	}
      /* If there is a '~', end the string; else put an "\r\n" over the '\n'. */
      if ((point = strchr (tmp, '~')) != NULL)
	{
	  *point = '\0';
	  done = 1;
	}
      else
	{
	  point = tmp + strlen (tmp) - 1;
	  *(point++) = '\r';
	  *(point++) = '\n';
	  *point = '\0';
	}

      templength = strlen (tmp);

      if (length + templength >= MAX_STRING_LENGTH)
	{
	  log ("SYSERR: fread_string: string too large (db.c)");
	  log (error);
	  exit (1);
	}
      else
	{
	  strcat (buf + length, tmp);
	  length += templength;
	}
    }
  while (!done);

  /* allocate space for the new string and copy it */
  if (strlen (buf) > 0)
    {
      CREATE (rslt, char, length + 1);
      strcpy (rslt, buf);
    }
  else
    rslt = NULL;

  return rslt;
}


/* release memory allocated for a char struct */
void
free_char (struct char_data *ch)
{
  int i;
  struct alias *a;
  void free_alias (struct alias *a);

  if (ch->player_specials != NULL && ch->player_specials != &dummy_mob)
    {
      while ((a = GET_ALIASES (ch)) != NULL)
	{
	  GET_ALIASES (ch) = (GET_ALIASES (ch))->next;
	  free_alias (a);
	}
      if (ch->player_specials->poofin)
	free (ch->player_specials->poofin);
      if (ch->player_specials->poofout)
	free (ch->player_specials->poofout);
      free (ch->player_specials);
      if (IS_NPC (ch))
	log ("SYSERR: Mob had player_specials allocated!");
    }
  if (!IS_NPC (ch) || (IS_NPC (ch) && GET_MOB_RNUM (ch) == -1))
    {
      /* if this is a player, or a non-prototyped non-player, free all */
      if (GET_NAME (ch))
	free (GET_NAME (ch));
      if (ch->player.title)
	free (ch->player.title);
      if (ch->player.short_descr)
	free (ch->player.short_descr);
      if (ch->player.long_descr)
	free (ch->player.long_descr);
      if (ch->player.description)
	free (ch->player.description);
      if (ch->player.passwd)
        free(ch->player.passwd);
    }
  else if ((i = GET_MOB_RNUM (ch)) > -1)
    {
      /* otherwise, free strings only if the string is not pointing at proto */
      if (ch->player.name && ch->player.name != mob_proto[i].player.name)
	free (ch->player.name);
      if (ch->player.title && ch->player.title != mob_proto[i].player.title)
	free (ch->player.title);
      if (ch->player.short_descr && ch->player.short_descr != mob_proto[i].player.short_descr)
	free (ch->player.short_descr);
      if (ch->player.long_descr && ch->player.long_descr != mob_proto[i].player.long_descr)
	free (ch->player.long_descr);
      if (ch->player.description && ch->player.description != mob_proto[i].player.description)
	free (ch->player.description);
    }
  while (ch->affected)
    affect_remove (ch, ch->affected);

  free (ch);
}




/* release memory allocated for an obj struct */
void
free_obj (struct obj_data *obj)
{
  int nr;
  struct extra_descr_data *this, *next_one;

  if ((nr = GET_OBJ_RNUM (obj)) == -1)
    {
      if (obj->name)
	free (obj->name);
      if (obj->description)
	free (obj->description);
      if (obj->short_description)
	free (obj->short_description);
      if (obj->action_description)
	free (obj->action_description);
      if (obj->ex_description)
	for (this = obj->ex_description; this; this = next_one)
	  {
	    next_one = this->next;
	    if (this->keyword)
	      free (this->keyword);
	    if (this->description)
	      free (this->description);
	    free (this);
	  }
    }
  else
    {
      if (obj->name && obj->name != obj_proto[nr].name)
	free (obj->name);
      if (obj->description && obj->description != obj_proto[nr].description)
	free (obj->description);
      if (obj->short_description && obj->short_description != obj_proto[nr].short_description)
	free (obj->short_description);
      if (obj->action_description && obj->action_description != obj_proto[nr].action_description)
	free (obj->action_description);
      if (obj->ex_description && obj->ex_description != obj_proto[nr].ex_description)
	for (this = obj->ex_description; this; this = next_one)
	  {
	    next_one = this->next;
	    if (this->keyword)
	      free (this->keyword);
	    if (this->description)
	      free (this->description);
	    free (this);
	  }
    }

  free (obj);
}



/* read contets of a text file, alloc space, point buf to it */
int
file_to_string_alloc (char *name, char **buf)
{
  char temp[MAX_STRING_LENGTH];

  if (*buf)
    free (*buf);

  if (file_to_string (name, temp) < 0)
    {
      *buf = "";
      return -1;
    }
  else
    {
      *buf = str_dup (temp);
      return 0;
    }
}


/* read contents of a text file, and place in buf */
int
file_to_string (char *name, char *buf)
{
  FILE *fl;
  char tmp[READ_SIZE + 3];

  *buf = '\0';

  if (!(fl = fopen (name, "r")))
    {
      sprintf (tmp, "SYSERR: reading %s", name);
      perror (tmp);
      return (-1);
    }
  do
    {
      fgets (tmp, READ_SIZE, fl);
      tmp[strlen (tmp) - 1] = '\0';	/* take off the trailing \n */
      strcat (tmp, "\r\n");

      if (!feof (fl))
	{
	  if (strlen (buf) + strlen (tmp) + 1 > MAX_STRING_LENGTH)
	    {
	      sprintf (buf, "SYSERR: %s: string too big (%d max)", name,
		       MAX_STRING_LENGTH);
	      log (buf);
	      *buf = '\0';
	      return -1;
	    }
	  strcat (buf, tmp);
	}
    }
  while (!feof (fl));

  fclose (fl);

  return (0);
}



/* clear some of the the working variables of a char */
void
reset_char (struct char_data *ch)
{
  int i;

  for (i = 0; i < NUM_WEARS; i++)
    GET_EQ (ch, i) = NULL;

  ch->followers = NULL;
  ch->master = NULL;
  ch->in_room = NOWHERE;
  ch->carrying = NULL;
  ch->next = NULL;
  ch->next_fighting = NULL;
  ch->next_in_room = NULL;
  FIGHTING (ch) = NULL;
  ch->char_specials.position = POS_STANDING;
  ch->mob_specials.default_pos = POS_STANDING;
  ch->char_specials.carry_weight = 0;
  ch->char_specials.carry_items = 0;

  if (GET_HIT (ch) <= 0)
    GET_HIT (ch) = 1;
  if (GET_MOVE (ch) <= 0)
    GET_MOVE (ch) = 1;
  if (GET_MANA (ch) <= 0)
    GET_MANA (ch) = 1;

  GET_LAST_TELL (ch) = NOBODY;
}



/* clear ALL the working variables of a char; do NOT free any space alloc'ed */
void
clear_char (struct char_data *ch)
{
  bzero (ch, sizeof (struct char_data));

  ch->in_room = NOWHERE;
  GET_WAS_IN (ch) = NOWHERE;
  GET_POS (ch) = POS_STANDING;
  ch->mob_specials.default_pos = POS_STANDING;

  if (ch->points.max_mana < 100)
    ch->points.max_mana = 100;
}


void
clear_object (struct obj_data *obj)
{
  memset ((char *) obj, 0, sizeof (struct obj_data));

  obj->item_number = NOTHING;
  obj->in_room = NOWHERE;
}




/* initialize a new character only if class is set */
void
init_char (struct char_data *ch)
{
  int i;

  /* create a player_special structure */
  if (ch->player_specials == NULL)
    CREATE (ch->player_specials, struct player_special_data, 1);

  /* *** if this is our first player --- he be God *** */

  if (top_of_p_table == 0)
    {
     GET_EXP (ch) = EXP_MAX;
     GET_LEVEL (ch) = LVL_IMPL;
     GET_TRUST_LEVEL(ch) = LVL_IMPL;

     ch->points.max_hit = 500;
     ch->points.max_mana = 100;
     ch->points.max_move = 82;

     set_title(ch, "the Implementor");

     GCMD_FLAGS(ch) = (~GCMD_CMDSET) | GCMD_FLAGS(ch);
     GCMD2_FLAGS(ch) = ~0;
     GCMD3_FLAGS(ch) = ~0;
     GCMD4_FLAGS(ch) = ~0;
  
     ch->real_abils.intel = 25;
     ch->real_abils.wis = 25;
     ch->real_abils.dex = 25;
     ch->real_abils.str = 25;  
     ch->real_abils.str_add = 100;
     ch->real_abils.con = 25;  
     ch->real_abils.cha = 25;
  }

  ch->player.short_descr = NULL;
  ch->player.long_descr = NULL;
  ch->player.description = NULL;

  ch->player.time.birth = time (0);
  ch->player.time.played = 0;
  ch->player.time.logon = time (0);

  for (i = 0; i < MAX_TONGUE; i++)
    GET_TALK (ch, i) = 0;

  /* make favors for sex */
  if (ch->player.sex == SEX_MALE)
    {
      ch->player.weight = number (120, 180);
      ch->player.height = number (160, 200);
    }
  else
    {
      ch->player.weight = number (100, 160);
      ch->player.height = number (150, 180);
    }

  ch->points.max_mana = 100;
  ch->points.mana = GET_MAX_MANA (ch);
  ch->points.hit = GET_MAX_HIT (ch);
  ch->points.max_move = 82;
  ch->points.move = GET_MAX_MOVE (ch);

  player_table[top_of_p_table].id = GET_IDNUM (ch) = ++top_idnum;

  for (i = 1; i <= MAX_SKILLS; i++)
    {
      if (GET_LEVEL (ch) < LVL_IMPL)
	SET_SKILL (ch, i, 0)
	  else
	SET_SKILL (ch, i, 100);
    }

  ch->char_specials.saved.affected_by = 0;

if (GET_LEVEL(ch) < LVL_IMPL) {
  /* Assign god commands */
     GCMD_FLAGS(ch) = 0;
     GCMD2_FLAGS(ch) = 0;
     GCMD3_FLAGS(ch) = 0;
     GCMD4_FLAGS(ch) = 0;
 }

  for (i = 0; i < 3; i++)
    GET_COND (ch, i) = (GET_LEVEL (ch) == LVL_IMPL ? -100 : 24);

  GET_COND(ch, DRUNK) = 0;
  GET_LOADROOM (ch) = NOWHERE;
}



/* returns the real number of the room with given virtual number */
/* Storm's SUPER SEXY binary search algorithm (works best with a whole SHITLOAD of rooms :)). */
int real_room (int virtual) {
  int i=top_of_world/2, low=0, high=top_of_world, x, firstpass=0;
  if (virtual < 0 || virtual > world[top_of_world].number) return NOWHERE;
  if (!i) return 0;
  if (ismap(top_of_world) || map_start_room == -1) {
    while (1) {
      x=world[i].number < virtual ? 1 : (world[i].number > virtual ? 0 : 2);
      switch (x) {
        case 0: 
          if (low==i || high==i) return NOWHERE;
          if (low==i-1) {
            high=i; i--;
          }
          else {
            high=i; i=(i+low)/2;
          }
          break;
 
        case 1:
          if (top_of_world==1) { /* Exception here and only here for only two rooms. */
            if (!firstpass) { i++; firstpass++; }
            else return NOWHERE;
            continue;
          }
          if (low==i || high==i) return NOWHERE;
          if (high==i+1) {
            low=i; i++;
          }
           else {
             low=i; i=(i+high)/2;
          }
          break;
  
        case 2: return i;
      }
    }
    return NOWHERE; /* Should never reach here, but just in case of some FREAK processor accident... */
  }
  for (low=0; low<top_of_world; low++)
    if (world[low].number == virtual)
      return low;
  return NOWHERE;
}



/* returns the real number of the monster with given virtual number */
int
real_mobile (int vnum)
{
  int bot, top, mid;

  bot = 0;
  top = top_of_mobt;

  /* perform binary search on mob-table */
  for (;;)
    {
      mid = (bot + top) / 2;

      if ((mob_index + mid)->vnum == vnum)
	return (mid);
      if (bot >= top)
	return (-1);
      if ((mob_index + mid)->vnum > vnum)
	top = mid - 1;
      else
	bot = mid + 1;
    }
}



/* returns the real number of the object with given virtual number */
int
real_object (int vnum)
{
  int bot, top, mid;

  bot = 0;
  top = top_of_objt;

  /* perform binary search on obj-table */
  for (;;)
    {
      mid = (bot + top) / 2;

      if ((obj_index + mid)->vnum == vnum)
	return (mid);
      if (bot >= top)
	return (-1);
      if ((obj_index + mid)->vnum > vnum)
	top = mid - 1;
      else
	bot = mid + 1;
    }
}

void read_mud_date_from_file(void)
{
   FILE *f;
   struct time_write read_date;

   f = fopen("etc/date_record", "r");
   if(!f) {
      log("SYSERR: File etc/date_record not found, mud date will be reset to default!");
      return;
   }

   fread(&read_date, sizeof(struct time_write), 1, f);
   time_info.year = read_date.year;
   time_info.month = read_date.month;
   time_info.day   = read_date.day;
   fclose(f);
}

void clean_pfile(void) { /* Removes all deleted players from the pfile (its a waste :P) */
  int i;
  MYSQL_RES *result;
  MYSQL_ROW row;

  sprintf(buf, "SELECT act,clan,clan_rank,idnum from player_main");
  QUERY_DATABASE(SQLdb, buf, strlen(buf));
  if (!(result=STORE_RESULT(SQLdb))) return;
  while ((row=FETCH_ROW(result)))
    if (IS_SET(ATOIROW(0), PLR_DELETED)) {
      delete_player_entry(ATOIROW(3));
      continue;
    }

  for (i=0;i<=top_of_p_table;i++) {
    if (player_table[i].name)
      free(player_table[i].name);
  }
  free(player_table);
  player_table=NULL; /* Woohoo :P */
  build_player_index();
  mysql_free_result(result);
}

ACMD(do_pfileclean) {
/*  FILE *pfile;
  struct char_data *newch;
  struct char_file_u newchf; */
  skip_spaces(&argument);
  if (strcmp(argument,"OptimisePfile")==0) {
    send_to_char("Cleaning Player File Now.\r\n",ch);
    sprintf(buf, "%s initiated playerfile clean.", GET_NAME(ch));
    mudlog(buf, NRM, LVL_IMPL, TRUE);
    clean_pfile();
  }
/*  else if (strcmp(argument,"ImportPfile")==0) {
    pfile=fopen("etc/players", "r");
    send_to_char("Importing Player File Now.\r\n",ch);
    QUERY_DATABASE(SQLdb, "DELETE from player_main", 23);
    while (fread(&newchf, sizeof (struct char_file_u), 1, pfile)) {
      CREATE (newch, struct char_data, 1);
      clear_char (newch);
      store_to_char(&newchf, newch);
      insert_player_entry(newch);
      free_char(newch);
    }
    send_to_char("Player File Imported. Shutting down.\r\n",ch);
    exit (0);
  }*/
  else
    send_to_char("Not unless you know the password.\r\n",ch);
}
