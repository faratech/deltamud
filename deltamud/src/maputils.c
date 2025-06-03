#define __MAPUTILS_C__

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
#include "maputils.h"

int real_zone(int number);

extern struct room_data *world;
extern int top_of_world;
extern int top_of_zone_table;
extern const char *room_bits[];
extern long r_mortal_start_room;
extern long r_immort_start_room;
extern const char *sector_types[];
extern struct zone_data *zone_table;
extern int rev_dir[];

char ***weather_map; /* Two dimensional array of pointers to w_cchars */
char **w_cchars;     /* One dimensional array of pointers to individual weather strings
                        like &BR (for rain) and &B> (for rain moving east)  */

const char *weather_chars[] = {
  "R",
  "S",
  "t",
  "F",
  "f",
  "M",
  "H",
  "T",
  "B",
  "D"
};

const char *weather_names[] = {
  "rain storm",
  "snow storm",
  "thunder storm",
  "fire storm",
  "fog",
  "magical fog",
  "hurricane",
  "tornado",
  "blizzard",
  "DEATH STORM"
};

const char *weather_colors[] = {
  "&B",
  "&W",
  "&g",
  "&r",
  "&c",
  "&C",
  "&G",
  "&R",
  "&w",
  "&M"
};

const char *weather_messages[] = {
  "A rain storm pours down on you from above.",
  "You tread heavily in the snow storm.",
  "You hear the blaring of the thunder storm above you and see lightning in the distance.",
  "Your already blackened skin is singed in the fire storm!",
  "You can barely see your hands in the heavy fog.",
  "You feel very uneasy in the strange fog.",
  "You attempt to hold your ground in the fierce hurricane!",
  "You are savagely hurled around by the tornado!",
  "Your limbs are chilled to the bone as a heavy blizzard looms above you.",
  "You cough blood and develop lesions as death encoils you."
};

const char *weather_corpse_names[] = {
  " ",
  " ",
  " ",
  "burnt crispy ",
  " ",
  " ",
  "torn apart ",
  "mangled ",
  "frozen solid ",
  "savagely ripped up "
};

int weather_data[WEATHER_TOTAL][5] = {
/*      speed   radius  damage  [percent chance it will change directions] [how long weather lasts in half-minutes]
  speed is how many rooms it goes per 30 SECONDS in a certain dir
  radius is how big it'll be - diamond shape
  damage is how much damage it does to each player per tic */

        {1,     3,      0,      5,      20},    // rain
        {1,     4,      0,      9,      48},    // snow
        {2,     4,      0,      13,     14},    // thunder
        {4,     2,      70,     20,     10},    // fire
        {0,     5,      0,      0,      16},    // fog
        {0,     5,      0,      0,      24},    // mfog
        {2,     5,      20,     15,     14},    // hurricane
        {3,     3,      10,     25,     12},    // tornado
        {1,     6,      7,      5,      96},     // blizzard
        {0,     10,     9999,   0,      96}     // DEATH
};

struct s_index *sect_index;
struct w_index *weather_index;
sh_int max_map_x=0;
sh_int max_map_y=0;
sh_int map_start_room=-1;
int num_weather;

void free_r (room_rnum rnum);
void death_cry (struct char_data *ch);
void write_aliases(struct char_data *ch);

void init_weather (void);
void send_weather_messages (struct w_index *w, int type);
int isinradius (int room, int room2, int radius);
void radial_activity (struct w_index *w);
void update_weather_map (void);
void weather_alloc (void);
void check_weather_collisions (void);
void reset_num_weather(void);

/* ----------------------- OUTSIDE MAP SYSTEM ----------------------- *
 * WARNING: DO NOT MODIFY ANYTHING BELOW UNLESS YOU *ABSOLUTELY* KNOW *
 * WHAT YOU ARE DOING! Any incorrect change can result in (but not    *
 * limited to) map corruption, MUD crashes, and zone/world file       *
 * corruption. I have done my best to make sure that none of this     *
 * happens but please, be careful.                                    *
 * ------------------------------------------------------------------ */

ACMD(do_togglemap) {
  skip_spaces(&argument);
  if (compare(argument, "on")) {
    if (MAP_ACTIVE) {
      send_to_char("No, no, no!\r\n", ch);
      return;
    }
    else {
      sprintf (buf, "%s has loaded the surface map.", GET_NAME(ch));
      mudlog (buf, NRM, LVL_GOD, TRUE);
      read_map();
    }
  }
  if (compare(argument, "off")) {
    if (!MAP_ACTIVE) {
      send_to_char("No, no, no!\r\n", ch);
      return;
    }
    else {
      send_to_char("NO! YOU'LL HURT SOMEONE!\r\n", ch);
      return;
      sprintf (buf, "%s has destroyed the surface map.", GET_NAME(ch));
      mudlog (buf, NRM, LVL_GOD, TRUE);
      unload_map();
    }
  }
}

void unload_map (void) {
  struct s_index *s=sect_index;
  struct char_data *ch;
  int i;
  room_rnum m=map_start_room;

  for (m=map_start_room; m<=(max_map_x*max_map_y)+(map_start_room)-1; m++) {
    while(world[m].people) {
      ch=world[m].people;
      if (IS_NPC(ch)) {
        extract_char(ch);
        continue;
      }
      send_to_char("\r\nThe ground below your feet suddenly crumbles to darkness.\r\n", ch);
      char_from_room(ch);
      sprintf(buf, "%s appears out of nowhere!\r\n", GET_NAME(ch));
      if (GET_LEVEL(ch) >= LVL_IMMORT) {
        send_to_room(buf, r_immort_start_room);
        char_to_room(ch, r_immort_start_room);
        look_at_room(ch, 1);
      }
      else {
        send_to_room(buf, r_mortal_start_room);
        char_to_room(ch, r_mortal_start_room);
        look_at_room(ch, 1);
      }
      // We would have world[m].people equal to the -next- person on the list, however, char_from_room took care of that for us already.
    }
    while (world[m].contents)
      extract_obj(world[m].contents);
    free_r(m);
    m++;
  }

 // Free map data and all map room descs, names, etc. (it would of all been cleared in the last loop, however, we have special rooms with independant descs) -- Also take care of the poor people that are in the map when it gets destroyed and purge all items in room.

  while(sect_index) {
    if (sect_index->show)
      free(sect_index->show);
    if (sect_index->name)
      free(sect_index->name);
    if (sect_index->desc)
      free(sect_index->desc);
    s=sect_index;
    sect_index=sect_index->next;
    free(s);
  } // Free sector data.

  RECREATE(world, struct room_data, map_start_room); // Shrink world[] back to its original size, freeing all the map rooms.
  sect_index=NULL;
  top_of_world=map_start_room-1;
  map_start_room=-1;
  max_map_x=0;
  max_map_y=0;
  for (i=0; i<=top_of_world; i++) { // Kill all entrypoints.
    world[i].linkrnum=-1;
    world[i].linkmapnum=-1;
    world[i].mapmv=-1;
  }
}

/* Chop of trailing \r's and \n's on strings, heh */
void JUDOCHOP(char *string) {
  int i=strlen(string)-1;
  while(string[i]=='\n' || string[i]=='\r') { string[i]=0; i--; }
}

void read_map (void) {
  FILE *mapfile;
  int mode=0, x=0, y=0, i=0, j=0, dir=-1, exitwhere, fl=0, n;
  sh_int rnum=-1, ernum;
  long virtual_map_start_room=2000000;
  long virtual_map_start_zone=real_zone(2000000);
  long beginmap;
  char buf[1002], arg[1002], desc[30001];
  struct s_index *s;
  map_start_room=-1;

  for (i=0; i<=top_of_world; i++) {
    world[i].linkrnum=-1;
    world[i].linkmapnum=-1;
    world[i].mapmv=-1;
    world[i].weather=-1;
  }
  i=0;
  sprintf(buf, "world/worldmap");
  if((mapfile=fopen(buf,"r"))==NULL)
    return;

  CREATE(sect_index, struct s_index, 1);
  s=sect_index;

  while (fgets(buf, 1001, mapfile)) {
    JUDOCHOP(buf);
    get_arg(buf, 1, arg);

    if ('~'==buf[0]) {
      if (mode==5) {
        world[rnum].description=strdup(desc);
        desc[0]='\0';
        mode=4;
      }
      if (mode==3) { 
        s->desc=strdup(desc);
        desc[0]='\0';
        mode=1;
      }
      if (mode==2) {
        mode=0;
      }
      continue;
    }

    switch (mode) {
      case 1:
        if (compare(arg, "SectShow:")) {
          if (buf[10]!=' ') {
            get_arg(buf, 2, arg);
            s->show=strdup(arg);
          }
          else
            s->show=strdup(" ");
          continue;
        }
        if (compare(arg, "SectName:")) {
          get_arg_exclude(buf, 1, arg);
          s->name=strdup(arg);
          continue;
        }
        if (compare(arg, "SectMove:")) {
          get_arg(buf, 2, arg);
          s->move=a2i(arg);
          continue;
        }
        if (compare(arg, "SectDesc:")) {
          desc[0]='\0';
          mode=3;
          continue;
        }
        if (compare(arg, "SectSect:")) {
          get_arg_exclude(buf, 1, arg);
          i=0;
          while (1) {
            if (compare((char *) sector_types[i], "\n")) break;
            if (compare((char *) sector_types[i], arg)) {
              s->sect=i;
            }
            i++;
          }
          continue;
        }
        break;
      
      case 2:
        for (i=0; i<strlen(buf); i++) {
          x++;
          s=find_sect_by_id(buf[i]);
          if (!s) {
            sprintf(desc, "MAPLOADER: Missing Sector Definition: {%c} @ (%d,%d)", buf[i], x, y);
            log(desc);
            exit(0);
          }
          if (map_start_room==-1)
            map_start_room=top_of_world+1;
          top_of_world++; 
          world[top_of_world].id=s->show;
          world[top_of_world].number=virtual_map_start_room;
          world[top_of_world].mapmv=s->move;
          world[top_of_world].zone=virtual_map_start_zone;
          world[top_of_world].name=s->name;
          world[top_of_world].linkrnum=-1;
          world[top_of_world].linkmapnum=-1;
          world[top_of_world].description=s->desc;
          world[top_of_world].sector_type=s->sect;
          world[top_of_world].people=NULL;
        }
        continue;
        break;
               
      case 3:
        strcat(desc, buf);
        strcat(desc, "\r\n");
        continue;
        break;

      case 4:
        if (compare(arg, "RoomShow:")) {
          get_arg(buf, 2, arg);
          world[rnum].id=strdup(arg);
          continue;
        }
        if (compare(arg, "RoomName:")) {
          get_arg_exclude(buf, 1, arg);
          world[rnum].name=strdup(arg);
          continue;
        }
        if (compare(arg, "RoomMove:")) {
          get_arg(buf, 2, arg);
          world[rnum].mapmv=a2i(arg);
          continue;
        }
        if (compare(arg, "RoomDesc:")) {
          desc[0]='\0';
          mode=5;
          continue;
        }
        if (compare(arg, "RoomSect:")) {
          get_arg_exclude(buf, 1, arg);
          i=0;
          while (1) {
            if (compare((char *) sector_types[i], "\n")) break;
            if (compare((char *) sector_types[i], arg)) {
              world[rnum].sector_type=i;
            }
            i++;
          }
          continue;
        }
        break;

      case 5:
        strcat(desc, buf);
        strcat(desc, "\r\n");
        continue;
        break;
    }

    if (compare(arg, "WorldMap:")) {
      beginmap=ftell(mapfile);
      x=max_map_x=max_map_y=0;
      while (fgets(buf, 1001, mapfile)) {
        JUDOCHOP(buf);
        if (buf[0]=='~') break;
        max_map_x=strlen(buf);
        if (fl && x!=max_map_x) { 
          sprintf(desc, "MAPLOADER: Map has inconsistent width!");
          log(desc);
          exit(0);
        }
        else
          fl=1;
        x=max_map_x;
        max_map_y++;
      }
      if (buf[0]!='~') {
        sprintf(desc, "MAPLOADER: Map has no terminating '~'!");
        log(desc);
        exit(0);
      }
      if (max_map_x == 0 || max_map_y == 0) {
        sprintf(desc, "MAPLOADER: Map is not drawn out!");
        log(desc);
        exit(0);
      }
      RECREATE(world, struct room_data, top_of_world+(max_map_x*max_map_y));
      bzero(world+top_of_world+1, sizeof(struct room_data)*max_map_x*max_map_y);
      fseek(mapfile, beginmap, SEEK_SET);
      mode=2;
      continue;
    }

    if (compare(arg, "NewSector:")) {
      if (s->next)
        s=s->next;
      mode=1;
      get_arg(buf, 2, arg);
      s->id=arg[0];
      continue;
    }

    if (compare(arg, "EndSector")) {
      mode=0;
      CREATE(s->next, struct s_index, 1);
      continue;
    }

    if (compare(arg, "SpecRoom:")) {
      mode=4;
      get_arg(buf, 2, arg);
      x=a2i(arg);
      get_arg(buf, 3, arg);
      y=a2i(arg);
      rnum=find_room_by_coords(x,y);
      if (rnum==NOWHERE)
        mode=0;
      continue;
    }

    if (compare(arg, "EndRoom")) {
      mode=0;
      continue;
    }

    if (compare(arg, "EntryPoint:")) {
      get_arg(buf, 2, arg);
      x=a2i(arg);
      get_arg(buf, 3, arg);
      y=a2i(arg);
      get_arg(buf, 4, arg);
      ernum=a2i(arg);
      if (find_room_by_coords(x,y)==NOWHERE || real_room(ernum)==NOWHERE)
        continue;

      get_arg(buf, 5, arg);

      if (is_abbrev(arg,"NORTH"))
        dir=NORTH;
      else if (is_abbrev(arg,"SOUTH"))
        dir=SOUTH;
      else if (is_abbrev(arg,"EAST"))
        dir=EAST;
      else if (is_abbrev(arg,"WEST"))
        dir=WEST;
      else if (is_abbrev(arg,"UP"))
        dir=UP;
      else if (is_abbrev(arg,"DOWN"))
        dir=DOWN;
      else {
        world[find_room_by_coords(x,y)].linkrnum=real_room(ernum);
        world[real_room(ernum)].linkmapnum=find_room_by_coords(x,y);
        continue;
      }

      if (!(world[find_room_by_coords(x,y)].dir_option[dir]))
        CREATE(world[find_room_by_coords(x,y)].dir_option[dir], struct room_direction_data, 1);
      world[find_room_by_coords(x,y)].dir_option[dir]->to_room=real_room(ernum);

      if (!(world[real_room(ernum)].dir_option[rev_dir[dir]]))
        CREATE(world[real_room(ernum)].dir_option[rev_dir[dir]], struct room_direction_data, 1);
      world[real_room(ernum)].dir_option[rev_dir[dir]]->to_room=find_room_by_coords(x,y);
      continue;
    }

    if (compare(arg, "ZWeatherPoint:")) {
      get_arg(buf, 2, arg);
      x=a2i(arg);
      get_arg(buf, 3, arg);
      y=a2i(arg);
      get_arg(buf, 4, arg);
      if (find_room_by_coords(x,y)==NOWHERE || real_room(a2i(arg))==NOWHERE)
        continue;
      for (n=0; n<=top_of_zone_table; n++)
        if (zone_table[n].number == a2i(arg)) {
          zone_table[n].ZWPointX=x;
          zone_table[n].ZWPointY=y;
        }
      world[find_room_by_coords(x,y)].wzonecontrol=a2i(arg);
      
      continue;
    }

    if (compare(arg, "BuildExit:")) {
      get_arg(buf, 2, arg);
      if (is_abbrev(arg,"FromRoomToMap"))
        exitwhere=1;
      else if (is_abbrev(arg,"FromMapToRoom"))
        exitwhere=2;
      else continue;

      get_arg(buf, 3, arg);

      if (is_abbrev(arg,"NORTH"))
        dir=NORTH;
      else if (is_abbrev(arg,"SOUTH"))
        dir=SOUTH;
      else if (is_abbrev(arg,"EAST"))
        dir=EAST;
      else if (is_abbrev(arg,"WEST"))
        dir=WEST;
      else if (is_abbrev(arg,"UP"))
        dir=UP;
      else if (is_abbrev(arg,"DOWN"))
        dir=DOWN;
      else continue;

      get_arg(buf, 4, arg);
      x=a2i(arg);
      get_arg(buf, 5, arg);
      y=a2i(arg);
      get_arg(buf, 6, arg);

      if (find_room_by_coords(x,y)==NOWHERE || real_room(a2i(arg))==NOWHERE)
        continue;
  
      if (exitwhere==1) {
        if (!(world[real_room(a2i(arg))].dir_option[dir]))
          CREATE(world[real_room(a2i(arg))].dir_option[dir], struct room_direction_data, 1);
        world[real_room(a2i(arg))].dir_option[dir]->to_room=find_room_by_coords(x,y);
      }
      if (exitwhere==2) {
        if (!(world[find_room_by_coords(x,y)].dir_option[dir]))
          CREATE(world[find_room_by_coords(x,y)].dir_option[dir], struct room_direction_data, 1);
        world[find_room_by_coords(x,y)].dir_option[dir]->to_room=real_room(a2i(arg));
      }
      continue;      
    }

    if (compare(arg, "FlagRoom:")) {
      get_arg(buf, 2, arg);
      x=a2i(arg);
      get_arg(buf, 3, arg);
      y=a2i(arg);
      rnum=find_room_by_coords(x,y);
      if (rnum==NOWHERE)
        continue;
      for (j=4; j<=count_args(buf); j++)
        for (i=0; room_bits[i]; i++) {
          if (compare((char *)room_bits[i],"\n"))
            break;
          get_arg(buf, j, arg);
          if (compare((char *)room_bits[i], arg))
            SET_BIT(world[rnum].room_flags, (1 << i));
        }
      continue;
    }
  }
  free(s->next);
  s->next=NULL;
  fclose(mapfile);
  buildmapexits();

 weather_alloc();
 init_weather(); // DO NOT PUT THIS ABOVE WEATHER_ALLOC!
}

#define WRAPX(x) \
  while (x>max_map_x) \
    x-=max_map_x; \
  while (x<1) \
    x+=max_map_x; \

#define WRAPY(y) \
  while (y>max_map_y) \
    y-=max_map_y; \
  while (y<1) \
    y+=max_map_y; \

room_rnum find_room_by_coords(sh_int x, sh_int y) {
  int rnum, nx=x, ny=y;
  if (!MAP_ACTIVE) return -1;
/* Lets wrap the map here... the world is ROUND! */
  WRAPX(nx);
  WRAPY(ny);
  rnum=max_map_x*(ny-1) + nx + map_start_room - 1;
  /* if (ismap(rnum)) we don't need this if the map wraps... */
  return rnum;
  /* return -1; zuh */
}

int ismap(room_rnum room) {
  if (!MAP_ACTIVE) return 0;
  if (room - (map_start_room) < 0 || (max_map_x*max_map_y)+(map_start_room)-1 < room )
    return 0;
  return 1;
} // Is room a maproom?

sh_int rm2x(room_rnum room) {
  sh_int r, x=0;
  if (!MAP_ACTIVE) return -1;
  r = room - (map_start_room);
  if (r<0)
    return -1;
  x=(r%max_map_x)+1;
  return x;
} // Tells you what X value a MAP room number has.

sh_int rm2y(room_rnum room) {
  sh_int r, y=0;
  if (!MAP_ACTIVE) return -1;
  r = room - (map_start_room);
  if (r<0)
    return -1;
  y=((r-(r%max_map_x))/max_map_x)+1;
  return y;
} // Tells you what Y value a MAP room number has.

struct s_index *find_sect_by_id(char id) {
  struct s_index *s;
  if (!sect_index) return NULL;
  for (s=sect_index; s; s=s->next) {
    if (s->id==id)
      return s;
  }
  return NULL;
}

ACMD(do_map) {
  int mode;
  sh_int x, y, xm, ym, xl, yl;
  room_rnum m, room;
  long len=10, i;
  char *mybuf;
  if (!MAP_ACTIVE) return;
// Args: map [world | weather] [[x1] [y1] [x2] [y2]]
  get_arg(argument, 1, buf);
  if (!strcasecmp(buf, "world"))
    mode=1;
  else if (!strcasecmp(buf, "weather"))
    mode=2;
  else
    mode=1;

  get_arg(argument, 2, buf);
  xm=MIN(MAX(atoi(buf), 1), max_map_x);
  get_arg(argument, 3, buf);
  ym=MIN(MAX(atoi(buf), 1), max_map_y);

  get_arg(argument, 4, buf);
  if (atoi(buf)==0)
    xl=MIN(xm+98, max_map_x);
  else
    xl=MIN(MAX(atoi(buf), xm), MIN(xm+98, max_map_x));
  get_arg(argument, 5, buf);
  if (atoi(buf)==0)
    yl=MIN(ym+98, max_map_y);
  else
    yl=MIN(MAX(atoi(buf), ym), MIN(ym+98, max_map_y));

  switch (mode) {
    case 2:
      send_to_char("Map of the World's Weather:\r\n", ch);
      for (y=ym; y<=yl; y++) {
        for (x=xm; x<=xl; x++) {
          m=find_room_by_coords(x,y);
          if (m==ch->in_room) {
            len+=3;
          }
          else {
            if (x>xm && weather_map[y-1][x-1][0] == '&' && !strncmp(weather_map[y-1][x-1], weather_map[y-1][(i=x-2) < 0 ? i+max_map_x : i], 2)) {
              len+=1;
            }
            else
              len+=strlen(weather_map[y-1][x-1]);
          }
        }
        len+=7;
      }
      mybuf=(char *) malloc(len);
      mybuf[0]='\0';

      for (y=ym; y<=yl; y++) {
        for (x=xm; x<=xl; x++) {
          m=find_room_by_coords(x,y);
          if (m==ch->in_room) {
            strcat(mybuf, "&n#");
          }
          else {
            if (x>xm && weather_map[y-1][x-1][0] == '&' && !strncmp(weather_map[y-1][x-1], weather_map[y-1][(i=x-2) < 0 ? i+max_map_x : i], 2)) {
              i=strlen(mybuf);
              mybuf[i]=weather_map[y-1][x-1][2];
              mybuf[i+1]='\0';
            }
            else
              strcat(mybuf, weather_map[y-1][x-1]);
          }
        }
        sprintf(mybuf+strlen(mybuf), "&n%3d", y);
        strcat(mybuf, "\r\n");
      }
      page_string(ch->desc, mybuf, 1);
      free(mybuf);
      break;
    default: 
      send_to_char("Map of the World:\r\n", ch);
      for (y=ym; y<=yl; y++) {
        for (x=xm; x<=xl; x++) {
          m=find_room_by_coords(x,y);
          if (m==ch->in_room) {
            len+=3;
          }
          else {
            if ((room=find_room_by_coords(x-1,y))!=-1 && x>xm &&
              world[room].id[0] == '&' && !strncmp(world[room].id, world[m].id, 2)) {
              len+=1;
            }
            else
              len+=strlen(world[m].id);
          }
        }
        len+=7;
      }
      mybuf=(char *) malloc(len);
      mybuf[0]='\0';

      for (y=ym; y<=yl; y++) {
        for (x=xm; x<=xl; x++) {
          m=find_room_by_coords(x,y);
          if (m==ch->in_room) {
            strcat(mybuf, "&n#");
          }
          else {
            if ((room=find_room_by_coords(x-1,y))!=-1 && x>xm &&
              world[room].id[0] == '&' && !strncmp(world[room].id, world[m].id, 2)) {
              i=strlen(mybuf);
              mybuf[i]=world[m].id[2];
              mybuf[i+1]='\0';
            }
            else
              strcat(mybuf, world[m].id);
          }
        }
        sprintf(mybuf+strlen(mybuf), "&n%3d", y);
        strcat(mybuf, "\r\n");
      }
      page_string(ch->desc, mybuf, 1);
      free(mybuf);
      break;
  }
}

void buildmapexits(void) { // Create exits to adjacent rooms; preserve existing exits.
  sh_int x, y;
  room_rnum m;

  for (y=1; y<=max_map_y; y++)
    for (x=1; x<=max_map_x; x++) {
      m=find_room_by_coords(x,y);
      if (!(world[m].dir_option[WEST])) {
        CREATE(world[m].dir_option[WEST], struct room_direction_data, 1);
        world[m].dir_option[WEST]->to_room=find_room_by_coords(x-1, y);
      }
      if (!(world[m].dir_option[EAST])) {
        CREATE(world[m].dir_option[EAST], struct room_direction_data, 1);
        world[m].dir_option[EAST]->to_room=find_room_by_coords(x+1, y);
      }
      if (!(world[m].dir_option[NORTH])) {
        CREATE(world[m].dir_option[NORTH], struct room_direction_data, 1);
        world[m].dir_option[NORTH]->to_room=find_room_by_coords(x, y-1);
      }
      if (!(world[m].dir_option[SOUTH])) {
        CREATE(world[m].dir_option[SOUTH], struct room_direction_data, 1);
        world[m].dir_option[SOUTH]->to_room=find_room_by_coords(x, y+1);
      }
    }
}

void get_arg(char *string, int argnum, char *arg) {
  int pos, j=1, k=0;
  *arg='\0';
  for (pos=0; pos<strlen(string); pos++)
    if (string[pos]!=' ') break; /* Skip any spaces in the beginning of the string */

  for (; pos<strlen(string); pos++) {
    if (string[pos]==' ') { j++; if (j==argnum+1) break; continue; }
    if (j!=argnum) continue;
    arg[k+1]=0;
    arg[k]=string[pos];
    k++;
  }
}

void get_arg_exclude(char *string, int argnum, char *arg) {
  int pos, j=1;
  char insert[5];
  *arg='\0';
  for (pos=0; pos<strlen(string); pos++)
    if (string[pos]!=' ') break; /* Skip any spaces in the beginning of the string */
  for (pos=0; pos<strlen(string); pos++) {
    if (string[pos]==' ') {
      if (j==argnum) {
        j++;
        continue;
      }
      else
        j++;
    }
    if (j==argnum) continue;
    *insert=string[pos];
    insert[1]='\0';
    strcat(arg, insert);
  }
  /* Remove all trailing whitespaces */
  for (pos=strlen(arg)-1; arg[pos]==' '; pos--)
    *(arg+pos)='\0';
}

int count_args(char *string) {
  int pos, j=1, i=1;
  char *string1;
  string1=strdup(string);
  for (pos=strlen(string1)-1; string1[pos]==' '; pos--)
    *(string1+pos)='\0';
  for (pos=0; pos<strlen(string1); pos++) {
    i=0;
    if (string[pos]==' ') j++;
  }
  free(string1);
  return j-i;
}

int compare(char *string1, char *string2) {
  int i;
  if (strlen(string1)!=strlen(string2))
    return 0;
  for (i=0; i<strlen(string1); i++)
    if (lowcase(string1[i])!=lowcase(string2[i]))
      return 0;
  return 1;
}

#define MAP_VISION_RADIUS_X 3
#define MAP_VISION_RADIUS_Y 3

char wmstr[4]="&K\0\0";
char wmstr2[4]="&K\0\0";

char *check_noroom(room_rnum i, struct char_data *ch, int radius, int modifier) {
  char *tmp, *tmp2;
  int j, rmbefore;

  if (i==-1)
    return "&n ";

  tmp=WEATHER_ACTIVE && AVOID_WEATHER(world[i].weather) && PRF2_FLAGGED(ch, PRF2_ADVANCEDMAP)
        ? ((wmstr[2]=weather_map[rm2y(i)-1][rm2x(i)-1][2]) ? wmstr : wmstr)
        : world[i].id;

  if (modifier==0)
    rmbefore=find_room_by_coords(rm2x(i)-1, rm2y(i));
  else
    rmbefore=find_room_by_coords(rm2x(i)+1, rm2y(i));

  tmp2=WEATHER_ACTIVE && AVOID_WEATHER(world[rmbefore].weather) && PRF2_FLAGGED(ch, PRF2_ADVANCEDMAP)
        ? ((wmstr2[2]=weather_map[rm2y(rmbefore)-1][rm2x(rmbefore)-1][2]) ? wmstr2 : wmstr2)
        : world[rmbefore].id;

  if (modifier==0) {
    j=rm2x(ch->in_room)-radius;
  }
  else {
    j=rm2x(ch->in_room)+radius;
  }

  if (!strncmp(tmp, tmp2, 2) && 
      (modifier == 0 ? (ch->in_room!=find_room_by_coords(rm2x(i)-1, rm2y(i))) : (ch->in_room!=find_room_by_coords(rm2x(i)+1, rm2y(i)))) &&
      tmp[0]=='&' &&
      (modifier == 0 ? ( j < 1 ? j+max_map_x : j ) : ( j > max_map_x ? j-max_map_x : j ))!=rm2x(i))
    return tmp+2;
  else
    return tmp;
}

#define MAP_INDENT " "
void printmap(int rnum, struct char_data * ch) {
  int j, k, sightradmult=2, invert=0;
  char buf[2000];
  if (map_start_room==-1 || !sect_index) return;
  if (GET_LEVEL(ch)>=LVL_IMMORT) sightradmult++;
  if (world[ch->in_room].weather == WEATHER_FOG && (GET_LEVEL(ch)<LVL_IMMORT || IS_NPC(ch))) {
    sightradmult--;
  }
  if (world[ch->in_room].weather == WEATHER_MAGICFOG && (GET_LEVEL(ch)<LVL_IMMORT || IS_NPC(ch))) {
    invert=1;
  }
  strcpy(buf, MAP_INDENT"&y+&n Map of Deltania &y+\r\n");
  strcat(buf, "&n.&c"); 
  for (j=-MAP_VISION_RADIUS_X*sightradmult; j<=MAP_VISION_RADIUS_X*sightradmult; j++)
    strcat(buf, "-");
  strcat(buf, "&n.\r\n");
  if (!invert)
    for (j=-(MAP_VISION_RADIUS_Y-(sightradmult == 1 ? 2 : 0)); j<=MAP_VISION_RADIUS_Y-(sightradmult == 1 ? 2 : 0); j++) {
      strcat(buf, "&c|");
      for (k=-MAP_VISION_RADIUS_X*sightradmult; k<=MAP_VISION_RADIUS_X*sightradmult; k++) {
        if (k==0 && j == 0) 
          strcat(buf, "&w#");
        else
          strcat(buf, check_noroom(find_room_by_coords(rm2x(ch->in_room)+k, rm2y(ch->in_room)+j), ch, MAP_VISION_RADIUS_X*sightradmult, invert));
      }
      strcat(buf, "&c|\r\n");
    }
  else
    for (j=MAP_VISION_RADIUS_Y; j>=-MAP_VISION_RADIUS_Y; j--) {
      strcat(buf, "&c|");
      for (k=MAP_VISION_RADIUS_X*sightradmult; k>=-MAP_VISION_RADIUS_X*sightradmult; k--) {
        if (k==0 && j == 0) 
          strcat(buf, "&w#");
        else
          strcat(buf, check_noroom(find_room_by_coords(rm2x(ch->in_room)+k, rm2y(ch->in_room)+j), ch, MAP_VISION_RADIUS_X*sightradmult, invert));
      }
      strcat(buf, "&c|\r\n");
    }
  strcat(buf, "`&c");
  for (j=-MAP_VISION_RADIUS_X*sightradmult; j<=MAP_VISION_RADIUS_X*sightradmult; j++)
    strcat(buf, "-");
  strcat(buf, "&n'\r\n");
  send_to_char(buf, ch);
}

char lowcase(char a) {
  return ((int) a >= 65 && (int) a <= 90 ? (char) ((int) a+32) : a);
}

char *rcds(room_rnum room) {
  sh_int i;
  static char mybuf[100];
  char a[100];

  if (ismap(room))
    sprintf(a, "%2dx%2d", rm2x(room), rm2y(room));
  else
    sprintf(a, "%5d", (int)world[room].number);
  for (i=0; i<strlen(a); i++)
    mybuf[i]=a[i];
  mybuf[strlen(a)]='\0';
  return mybuf;
}
/*
room_rnum newroom(void) {
  room_rnum rnum;
  struct m_index *m;
  RECREATE(world, struct room_data, top_of_world+2);
  top_of_world++;
  if (map_start_room==-1)
    return top_of_world;
  else {
    map_start_room++;
    while(rnum >= 0) {
      if (!ismap(rnum)) {
        clear_room(rnum+1);
        return rnum+1;
      }
      world[rnum+1]=world[rnum];
      rnum--;
    }
  }
  return -1;
}

void clear_room(room_rnum room) {
  int i;
  world[room].name=NULL;
  world[room].description=NULL;
  world[room].special_exit=NULL;
  for (i = 0; i < NUM_OF_DIRS; i++) {
    world[room].dir_option[i]=NULL;
  }
  world[room].ex_description=NULL;
  world[room].number=-1;
  world[room].zone=-1;
  world[room].sector_type=0;
  world[room].room_flags=0;
  world[room].linkmapnum=-1;
  world[room].linkrnum=-1;
  world[room].id=NULL;
  world[room].mapmv=-1;
  world[room].light=0;
  world[room].blood=0;
  world[room].snow=0;
  world[room].proto_script=NULL;
  world[room].script=NULL;
  world[room].contents=NULL;
  world[room].people=NULL;
}
This code was intended to make *new* rooms UNDER the map -- but since redit seems to take care of that we
just commented it out (didn't want to waste code with future potential).
*/

#define ISNUMBER(a) (a >= '0' && a <= '9')

room_rnum cdsr(char *string) {
  int i, xf=0, l, x=-1;
  char tmp[MAX_STRING_LENGTH]="\0";

  for (i=0; i<strlen(string); i++) {
    if (ISNUMBER(string[i])) {
      l=strlen(tmp);
      tmp[l]=string[i];
      tmp[l+1]='\0';
    }
    else if (string[i]=='x' && !xf) {
      if (strlen(tmp)<1) return -1; // No X coordinate supplied, return noroom.
      xf=1;
      x=a2i(tmp);
      tmp[0]='\0';
    }
    else return -1;
  }
  if (strlen(tmp)<1) return -1; // No Y coordinate supplied after the 'x', return noroom.
  if (!xf) return real_room(a2i(tmp)); // If the argument was completely a number, assume its a vnum and return the rnum.
  if (x < 1 || x > max_map_x || a2i(tmp) < 1 || a2i(tmp) > max_map_y) return -1; /* Old code would wrap 500x500 into a smaller 99x99 map, we don't want that. */
  return find_room_by_coords(x, a2i(tmp));
}

void free_r (room_rnum rnum) {
  int i, noname=0, nodesc=0;
  struct extra_descr_data *t;
  struct s_index *s=sect_index;

  while (s) {
    if (s->name==world[rnum].name)
      noname=1;
    if (s->desc==world[rnum].description)
      nodesc=1;
    s=s->next;
  }

  if (world[rnum].name && !noname)
    free(world[rnum].name);
  else
    world[rnum].name=NULL;

  if (world[rnum].description && !nodesc)
    free(world[rnum].description);
  else
    world[rnum].description=NULL;

  if (world[rnum].special_exit) {
    if (world[rnum].special_exit->general_description)
      free(world[rnum].special_exit->general_description);
    if (world[rnum].special_exit->keyword)
      free(world[rnum].special_exit->keyword);
    if (world[rnum].special_exit->ex_name)  
      free(world[rnum].special_exit->ex_name);
    if (world[rnum].special_exit->leave_msg)
      free(world[rnum].special_exit->leave_msg);
    free(world[rnum].special_exit); 
  }

  for (i = 0; i < NUM_OF_DIRS; i++) {
    if (world[rnum].dir_option[i]) {
      if (world[rnum].dir_option[i]->general_description)
        free(world[rnum].dir_option[i]->general_description);
      if (world[rnum].dir_option[i]->keyword)
        free(world[rnum].dir_option[i]->keyword);
    }
    free(world[rnum].dir_option[i]);
    world[rnum].dir_option[i]=NULL;
  }

  while(world[rnum].ex_description) {
    if (world[rnum].ex_description->keyword)
      free(world[rnum].ex_description->keyword);
    if (world[rnum].ex_description->description)
      free(world[rnum].ex_description->description);
    t=world[rnum].ex_description;
    world[rnum].ex_description=world[rnum].ex_description->next;
    free(t);
  }
}

/* ------------------------- WEATHER SYSTEM ------------------------- *
 * The following code contains procedures for spawning, printing, and *
 * controlling different weather on the surface map.                  *
 * ------------------------------------------------------------------ */

struct w_index *spawn_weather (int type, int dir, room_rnum rnum) {
  struct w_index *w;

  if (!MAP_ACTIVE) return NULL;

  if (dir < 0 || dir > 3) {
    dir = number(0, 3); // If no valid direction is supplied, figure out a random one.
  }

  if (type < 0 || type > WEATHER_TOTAL)
    return NULL;

  if (!ismap(rnum))
    return NULL;

  if (!weather_index) {
    CREATE(weather_index, struct w_index, 1);
    w=weather_index;
  }
  else {
    for (w=weather_index; w->next;) w=w->next;
    CREATE(w->next, struct w_index, 1);
    w=w->next;
  }
  num_weather++;
  w->type=type;
  w->in_room=rnum;
  w->left=weather_data[type][4];
  w->dir=dir;
  w->radius=weather_data[type][1];
//  radial_activity(w); Weather is spawning here... give everyone here and around it a warning before actually showing its effects.
  send_weather_messages(w, WEATHER_MSG_FORM);
  return w;
}

void destroy_weather (int num) {
  struct w_index *w, *lastw=NULL;
  int i=0;

  if (!WEATHER_ACTIVE || num < 1 || !MAP_ACTIVE) return;
  if (num==1) {
    w=weather_index;
    send_weather_messages(w, WEATHER_MSG_STOP);
    weather_index=weather_index->next;
    free(w);
    reset_num_weather();
    return;
  }
  for (w=weather_index; w; w=w->next) {
    i++;
    if (i==num) {
      send_weather_messages(w, WEATHER_MSG_STOP);
      lastw->next=w->next;
      free(w);
      reset_num_weather();
      return;
    }
    lastw=w;
  }
  return;
}

int rand_weather (void) {
  int i;
  i=number(1, 100);
  if (i>=80)
    return WEATHER_RAINSTORM;
  if (i>=60)
    return WEATHER_THUNDERSTORM;
  if (i>=40)
    return WEATHER_FOG;
  // Rain/ThunderStorms And Fogs each have 20% chance of loading...

  if (i>=30)
    return WEATHER_MAGICFOG;
  if (i>=20)
    return WEATHER_SNOWSTORM;
  // Magical Fogs and Snow storms only occur 10% of the time.

  switch (number(1, 4)) {
    case 1: return WEATHER_BLIZZARD;
    case 2: return WEATHER_FIRESTORM;
    case 3: return WEATHER_HURRICANE;
    case 4: return WEATHER_TORNADO;
  }
  //  All damaging weathers only have a 5% chance of loading. Collectively there's a 20% chance a damaging weather will load.

  return WEATHER_RAINSTORM;
  // We should never get here but oh well...
}

void init_weather (void) {
  int i;
  struct w_index *w;
  if (map_start_room==-1) return;
  for (i=1; i<=MAX_WEATHER; i++)
    w=spawn_weather(rand_weather(), -1, number(map_start_room, top_of_world));
  update_weather_map();
  check_weather_collisions ();
}

int wrap_method_x (int x, int method) {
  switch (method) {
    case WRAP_NORM_NORM: return x;
    case WRAP_NORM_LESS: return x;
    case WRAP_NORM_PLUS: return x;
    case WRAP_PLUS_NORM: return x+max_map_x;
    case WRAP_PLUS_LESS: return x+max_map_x;
    case WRAP_PLUS_PLUS: return x+max_map_x;
    case WRAP_LESS_NORM: return x-max_map_x;
    case WRAP_LESS_LESS: return x-max_map_x;
    case WRAP_LESS_PLUS: return x-max_map_x;
    default: return 0;
  }
}
int wrap_method_y (int y, int method) {
  switch (method) {
    case WRAP_NORM_NORM: return y;
    case WRAP_NORM_LESS: return y-max_map_y;
    case WRAP_NORM_PLUS: return y+max_map_y;
    case WRAP_PLUS_NORM: return y;
    case WRAP_PLUS_LESS: return y-max_map_y;
    case WRAP_PLUS_PLUS: return y+max_map_y;
    case WRAP_LESS_NORM: return y;
    case WRAP_LESS_LESS: return y-max_map_y;
    case WRAP_LESS_PLUS: return y+max_map_y;
    default: return 0;
  }
}

void check_weather_collisions (void) { // Ah, now we're starting to get more realistic :)
  struct w_index *w, *tw, *temp;
  sh_int x, y, type, rad, left, n, smode=0 /* , gmode=0 */;
  if (map_start_room==-1) return;
  while (1) {
    n=0;
    x=0;
    y=0;
    type=0;
    rad=0;
    w=NULL;
    tw=NULL;
    temp=NULL;
    for (w=weather_index; w; w=w->next) {
      for (tw=weather_index; tw; tw=tw->next) {
        if (w==tw)
          continue;
        if ( (smode=isinradius_wrap(rm2x(w->in_room), rm2y(w->in_room), rm2x(tw->in_room), rm2y(tw->in_room), tw->radius)) /* ||
             (gmode=isinradius_wrap(rm2x(tw->in_room), rm2y(tw->in_room), rm2x(w->in_room), rm2y(w->in_room), w->radius)) */) {
          // Map wrapping here calls the same function NINE times TWICE, thats 18 isinradius calls *per weather occurence*.
          // smode set to anything above 0 tells us call #1 passed, and its output.
          // gmode set to anything above 0 tells us call #2 passed, and its output.


/* Somebody check the above two lines, do we really need to call that function twice?
   I think it might be a little redundant checking x against y and then y against x, when
   the y against x check will be passed to it eventually, gonna comment gmode out, hope nothing
   terrible happens. -Storm '00*/

          if (smode) {
            x=(wrap_method_x(rm2x(w->in_room), smode)+rm2x(tw->in_room))/2;
            y=(wrap_method_y(rm2y(w->in_room), smode)+rm2y(tw->in_room))/2;
            smode=0;
          }
/*          if (gmode) {
            x=(wrap_method_x(rm2x(tw->in_room), gmode)+rm2x(w->in_room))/2;
            y=(wrap_method_y(rm2y(tw->in_room), gmode)+rm2y(w->in_room))/2;
            gmode=0;
          } */
          // Find the midpoint.

          type=number(1, w->radius+tw->radius);

          if (type <= w->radius)
            type=w->type;
          else
            type=tw->type;
          // Come to a type between the two based on which one is bigger.

          rad=MIN(w->radius+tw->radius, (max_map_x+max_map_y)/2);
          // Take their collective radius... And make sure its no bigger then the avg of the x and y limits of the map (so the maximum we can have is a storm that covers every spot on the map wherever its center is - STORM OF THE GODS :P)

          left=w->left+tw->left;
          // Take the collective time till expiration ...

          REMOVE_FROM_LIST(w, weather_index, next);
          free(w);
          // Remove the first weather type, do not change the number of weathers (we don't want new storms spawning until this one runs out).

          tw->type=type;
          tw->dir=number(0, 3);
          tw->in_room=find_room_by_coords(x,y);
          tw->radius=rad;
          tw->left=left;
          // Change the second weather type to the 'new' weather.
          reset_num_weather();
          n=1; break;
        }
      }
      if (n==1) break;
    }

    if (n==1)
      continue;
    else
      break;
    // Cycle through the loop until there are no more weathers within a certain radius of each other.
  }
}

void make_weather_corpse (struct char_data *ch, int j) { // Just like make_corpse except for weather... special messages
  struct obj_data *corpse, *o;
  struct obj_data *money;
  int i;
  extern int max_npc_corpse_time, max_pc_corpse_time;

  struct obj_data *create_money (int amount);

  corpse = create_obj ();

  corpse->item_number = NOTHING;
  corpse->in_room = NOWHERE;
  corpse->name = str_dup ("corpse");

  sprintf (buf2, "The %scorpse of %s is lying here.", weather_corpse_names[j][0] != ' ' ? weather_corpse_names[j] : "", GET_NAME (ch));
  corpse->description = str_dup (buf2);

  sprintf (buf2, "the %scorpse of %s", weather_corpse_names[j][0] != ' ' ? weather_corpse_names[j] : "", GET_NAME (ch));
  corpse->short_description = str_dup (buf2);

  GET_OBJ_TYPE (corpse) = ITEM_CONTAINER;
  GET_OBJ_WEAR (corpse) = ITEM_WEAR_TAKE;
  GET_OBJ_EXTRA (corpse) = ITEM_NODONATE;
  GET_OBJ_VAL (corpse, 0) = 0;  /* You can't store stuff in a corpse */
  GET_OBJ_VAL (corpse, 3) = 1;  /* corpse identifier */
  GET_OBJ_WEIGHT (corpse) = GET_WEIGHT (ch) + IS_CARRYING_W (ch);
  GET_OBJ_RENT (corpse) = 100000;
  if (IS_NPC (ch))
    GET_OBJ_TIMER (corpse) = max_npc_corpse_time;
  else
    GET_OBJ_TIMER (corpse) = max_pc_corpse_time;

  /* transfer character's inventory to the corpse */
  corpse->contains = ch->carrying;
  for (o = corpse->contains; o != NULL; o = o->next_content)
    o->in_obj = corpse;
  object_list_new_owner (corpse, NULL);

  /* transfer character's equipment to the corpse */
  for (i = 0; i < NUM_WEARS; i++)
    if (GET_EQ (ch, i))
      obj_to_obj (unequip_char (ch, i), corpse);

  /* transfer gold */
  if (GET_GOLD (ch) > 0)
    {
      /* following 'if' clause added to fix gold duplication loophole */
      if (IS_NPC (ch) || (!IS_NPC (ch) && ch->desc))
        {
          money = create_money (GET_GOLD (ch));
          obj_to_obj (money, corpse);
        }
      GET_GOLD (ch) = 0;
    }
  ch->carrying = NULL;
  IS_CARRYING_N (ch) = 0;
  IS_CARRYING_W (ch) = 0;

  obj_to_room (corpse, ch->in_room);
}

void move_char (struct char_data *ch, int length, int dir) {
  int i, nr, d1=0, d2=0, hit=0, j=0;
  char mybuf[100], mybuf2[100], mybuf3[100];
  room_rnum start_room;
  struct char_data *tch=NULL;
  if (dir<0 || dir>7) return;
  if (!PRF2_FLAGGED(ch, PRF2_NOMAP)) {
    SET_BIT(PRF2_FLAGS(ch), PRF2_NOMAP);
    j=1;
  }
  start_room=find_room_by_coords(ismap(ch->in_room) ? rm2x(ch->in_room) : zone_table[world[ch->in_room].zone].ZWPointX, ismap(ch->in_room) ? rm2y(ch->in_room) : zone_table[world[ch->in_room].zone].ZWPointY);
  char_from_room(ch);
  char_to_room(ch, start_room);
  sprintf(mybuf, "You duck as you see %s fly by your head screaming!\r\n", GET_NAME(ch));
  switch (dir) {
    case 4: // NorthEast
      d1=NORTH; d2=EAST; break;
    case 5: // NorthWest
      d1=NORTH; d2=WEST; break;
    case 6: // SouthEast
      d1=SOUTH; d2=EAST; break;
    case 7: // SouthWest
      d1=SOUTH; d2=WEST; break;
  }
  for (i=1; i<=length; i++) {
    if (dir>=0 && dir<4 && CAN_GO(ch, dir)) {
      nr=world[ch->in_room].dir_option[dir]->to_room;
      if (nr==NOWHERE) continue;
      char_from_room(ch);
      if (i!=length) {
        if ((tch=world[nr].people) != NULL && GET_LEVEL(tch)<LVL_IMMORT && number(1, 100)<=10) {
          char_from_room(tch);
          sprintf(mybuf2, "You gasp but are too late to dodge an oncoming %s and the impact knocks you unconscious!\r\n", GET_NAME(ch));
          send_to_char(mybuf2, tch);
          sprintf(mybuf3, "%s watches in horror as %s comes flying into %s!\r\n%s is knocked unconscious by the impact and %s keeps going!\r\n", GET_NAME(tch), GET_NAME(ch), HMHR(tch), GET_NAME(tch), GET_NAME(ch));
          send_to_room(mybuf3, nr);
          char_to_room(tch, nr);
          hit=1;
        }
        else
          send_to_room(mybuf, nr);
      }
      char_to_room(ch, nr);
      look_at_room(ch, 0);
      if (hit) {
        send_to_char("You hit something but everything speeds by you too quickly to figure out what it was!\r\n", ch);
        GET_POS(tch)=POS_SLEEPING;
        hit=0;
      }
      continue;
    }
    if (CAN_GO(ch, d1))
      nr=world[ch->in_room].dir_option[d1]->to_room;
    else
      break;
    if (nr==NOWHERE) break;
    if (world[nr].dir_option[d2])
      nr=world[nr].dir_option[d2]->to_room;
    else
      break;
    char_from_room(ch);
    if (i!=length) {
      if ((tch=world[nr].people) != NULL && GET_LEVEL(tch)<LVL_IMMORT && number(1, 100)<=10) {
        char_from_room(tch);
        GET_POS(tch)=POS_SLEEPING;
        sprintf(mybuf2, "You gasp but are too late to dodge an oncoming %s and the impact knocks you unconscious!\r\n", GET_NAME(ch));
        send_to_char(mybuf2, tch);
        sprintf(mybuf3, "%s watches in horror as %s comes flying into %s!\r\n%s is knocked unconscious by the impact and %s keeps going!\r\n", GET_NAME(tch), GET_NAME(ch), HMHR(tch), GET_NAME(tch), GET_NAME(ch));
        send_to_room(mybuf3, nr);
        char_to_room(tch, nr);
        hit=1;
      }
      else 
        send_to_room(mybuf, nr);
    }
    char_to_room(ch, nr);
    look_at_room(ch, 0);
    if (hit) {
      send_to_char("You hit something but everything speeds by you too quickly to figure out what it was!\r\n", ch);
      GET_POS(tch)=POS_SLEEPING;
      hit=0;
    }
  }
  if (j)
    REMOVE_BIT(PRF2_FLAGS(ch), PRF2_NOMAP);
}

void weather_mprand (struct char_data *ch, int length) { /* This function will throw a player length rooms in a random direction on the surface map */
  int i, attempt, x=ismap(ch->in_room) ? rm2x(ch->in_room) : zone_table[world[ch->in_room].zone].ZWPointX, y=ismap(ch->in_room) ? rm2y(ch->in_room) : zone_table[world[ch->in_room].zone].ZWPointY, stop=0;
  for (i = 0; i < 7; i++) {
    attempt=number(0,7);
    switch (attempt) {
      case 0: // North
        if (y-length<1) continue;
        move_char(ch, length, NORTH);
        stop=1; break;
      case 1: // South
        if (y+length>max_map_y) continue;
        move_char(ch, length, SOUTH);
        stop=1; break;
      case 2: // East
        if (x+length>max_map_x) continue;
        move_char(ch, length, EAST);
        stop=1; break;
      case 3: // West
        if (x-length<1) continue;
        move_char(ch, length, WEST);
        stop=1; break;
      case 4: // NorthEast
        if (y-(length/2)<1 || x+(length/2)>max_map_x) continue;
        move_char(ch, length, 4);
        stop=1; break;
      case 5: // NorthWest
        if (y-(length/2)<1 || x-(length/2)<1) continue;
        move_char(ch, length, 5);
        stop=1; break;
      case 6: // SouthEast
        if (y+(length/2)>max_map_y || x+(length/2)>max_map_x) continue;
        move_char(ch, length, 6);
        stop=1; break;
      case 7: // SouthWest
        if (y+(length/2)>max_map_y || x-(length/2)<1) continue;
        move_char(ch, length, 7);
        stop=1; break;
    }
    if (stop) break;
  }
}

void send_to_radius (char *string, room_rnum in_room, int radius) {
  int x, y, rm, j, k;
  if (!MAP_ACTIVE) return;
  for (y=rm2y(in_room)-radius; y<=rm2y(in_room)+radius; y++)
    for (x=rm2x(in_room)-radius; x<=rm2x(in_room)+radius; x++) {
      rm=find_room_by_coords(x, y);
      if (isinradius_bycoords(x,y,rm2x(in_room),rm2y(in_room),radius)) {
        if (!ROOM_FLAGGED(rm, ROOM_INDOORS) && world[rm].people)
          send_to_room(string, rm);
        if (world[rm].wzonecontrol!=0)
          for (j=0; j<=top_of_zone_table; j++)
            if (zone_table[j].number == world[rm].wzonecontrol) {
              for (k=real_room(j*100); world[k].zone==j; k++)
                if (!ROOM_FLAGGED(k, ROOM_INDOORS) && world[k].people)
                  send_to_room(string, k);
              break;
            }
      }
    }
}

void weather_show_pos (struct char_data *ch, int type) {
            switch (GET_POS (ch)) {
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
                send_to_char ("You are dead!  Sorry...\r\n", ch);
                if (!IS_NPC (ch)) {
                  sprintf (buf2, "%s killed by weather at %s", GET_NAME(ch), world[ch->in_room].name);
                  mudlog (buf2, BRF, LVL_IMMORT, TRUE);
                  REMOVE_BIT (PLR_FLAGS (ch), PLR_KILLER | PLR_THIEF);
                  write_aliases(ch);
                  GET_COND(ch, FULL) = 0;
                  GET_COND(ch, THIRST) = 0;
                  GET_COND(ch, DRUNK) = 0;
                }
                if (FIGHTING (ch))
                stop_fighting (ch);
                while (ch->affected)
                  affect_remove (ch, ch->affected);
                death_cry(ch);
                make_weather_corpse(ch, type);
                extract_char(ch);
                break;
            }
}

void unit_activity (room_rnum room, struct w_index *w) { 
  static struct affected_type *af;
  struct char_data *ch, *nextch=NULL;
  int struck = 0;
  room_rnum oldroom;
  if (!MAP_ACTIVE) return;

      for (ch=world[room].people; ch; ch=nextch) {
        nextch=ch->next_in_room;

/* Mike 6/28/00 */
  if (IS_NPC(ch)) return;

        if (IS_NPC(ch) || (GET_LEVEL(ch) < LVL_IMMORT)) {
          if (w->type == WEATHER_MAGICFOG && number(1, 5)==1) {
            switch (number(1,6)) {
              case 1: command_interpreter(ch, "sneeze"); break;
              case 2: command_interpreter(ch, "scream"); break;
              case 3: command_interpreter(ch, "hiccup"); break;
              case 4: command_interpreter(ch, "heh"); break;
              case 5: command_interpreter(ch, "slap self"); break;
              case 6: command_interpreter(ch, "emote shakes and quivers like a bowlfull of jelly."); break;
            }
          }
          if (w->type == WEATHER_THUNDERSTORM && struck==0 && number(1, 4)==1) {
            act ("You see a holy bolt of lightning discharge from the sky!\r\nThe SHOCKING moment fries $n to a crisp!", TRUE, ch, 0, 0, TO_ROOM);
            send_to_char ("You see a holy bolt of lightning discharge from the sky in your direction!\r\n&CZZZZZZZZZZZZT&n!!\r\n", ch);
            struck = 1;
            if (AFF_FLAGGED(ch, AFF_REDIRECT_CHARGE)) {
              REMOVE_BIT(AFF_FLAGS(ch), AFF_REDIRECT_CHARGE);
              for (af = ch->affected; af; af = af->next)
                if (af->type==SPELL_REDIRECT_CHARGE) {
                  SET_BIT(AFF_FLAGS(ch), AFF_R_CHARGED);
                  af->bitvector=AFF_R_CHARGED;
                  af->modifier=number(400, 900);
                  af->location=APPLY_DAMAGE;
                  GET_HIT(ch)=GET_HIT(ch)-(af->modifier/16/(IS_AFFECTED(ch, AFF_SANCTUARY) ? 2 : 1));
                  af->modifier=(af->modifier*15)/16;
                  af->duration=100;
                  act ("$n amazingly absorbs the bolt of energy!", TRUE, ch, 0, 0, TO_ROOM);
                  send_to_char ("You feel the godly charge run through your body and your magic contains it!\r\n", ch);
                  break;
                }
            }
            else
              GET_HIT(ch)=GET_HIT(ch)-(number(400, 900)/(IS_AFFECTED(ch, AFF_SANCTUARY) ? 2 : 1));
            update_pos (ch);
            weather_show_pos (ch, w->type);
            if (GET_POS(ch) != POS_DEAD) send_to_char("You feel a little bit crispier.\r\n", ch); 
            else continue;
          }
          if (weather_data[w->type][2]>0) {
            send_to_char("&RYou are wounded by the ", ch);
            send_to_char((char *)weather_names[w->type], ch);
            send_to_char("!&n\r\n", ch);
            if (IS_AFFECTED(ch, AFF_SANCTUARY) && weather_data[w->type][2] >= 2)
              GET_HIT(ch)=GET_HIT(ch)-(weather_data[w->type][2]/2);
            else
              GET_HIT(ch)=GET_HIT(ch)-weather_data[w->type][2];
            update_pos (ch);
            weather_show_pos (ch, w->type);
            if (GET_POS(ch) == POS_DEAD) continue;
          }
          // We start our weather throwing procedures here...
          if (number(1, MIN(MAX(GET_WEIGHT(ch), 120), 160))<=70 && (w->type == WEATHER_HURRICANE || w->type == WEATHER_TORNADO)) {
// Checks passed... LIFTOFF!#!@$$!
            GET_WAS_IN(ch)=ismap(ch->in_room) ? ch->in_room : find_room_by_coords(zone_table[world[ch->in_room].zone].ZWPointX, zone_table[world[ch->in_room].zone].ZWPointY);
            sprintf(buf, "The %s jettisons $n into the air!", (char *)weather_names[w->type]);
            act (buf, FALSE, ch, 0, 0, TO_ROOM);
            sprintf(buf, "The %s jettisons you into the air!\r\n", (char *)weather_names[w->type]);
            send_to_char(buf, ch);
            weather_mprand(ch, w->type == WEATHER_HURRICANE ? 12 : 8);
            oldroom=ch->in_room;
            char_from_room(ch);
            sprintf(buf, "You see %s flying through the air in the distance.\r\n", GET_NAME(ch));
            send_to_radius(buf, find_room_by_coords((rm2x(oldroom)+rm2x(GET_WAS_IN(ch)))/2, (rm2y(oldroom)+rm2y(GET_WAS_IN(ch)))/2), w->radius*2);
            sprintf(buf, "%s falls from the sky landing head-first into the ground!\r\n", GET_NAME(ch));
            send_to_char("You land head-first into the ground!\r\n", ch);
            send_to_room(buf, oldroom);
            GET_WAS_IN(ch)=-1;
            GET_POS(ch)=POS_RESTING;
            char_to_room(ch, oldroom);
          }
        }
      }
}

void radial_activity (struct w_index *w) {
  int x, y, room=w->in_room;
  int j;
  room_rnum i, k;
  if (!MAP_ACTIVE) return;
  for (y=rm2y(room)-w->radius; y<=rm2y(room)+w->radius; y++)
    for (x=rm2x(room)-w->radius; x<=rm2x(room)+w->radius; x++) {
      if (!isinradius_bycoords(x, y, rm2x(room), rm2y(room), w->radius) || !ismap((i=find_room_by_coords(x, y))) || (!world[i].people && world[i].wzonecontrol==0)) continue;
      if (!ROOM_FLAGGED(i, ROOM_INDOORS))
        unit_activity(i, w);
      if (world[i].wzonecontrol!=0)
        for (j=0; j<=top_of_zone_table; j++) {
          if (zone_table[j].number == world[i].wzonecontrol) {
            for (k=real_room(j*100); world[k].zone==j; k++)
              if (!ROOM_FLAGGED(k, ROOM_INDOORS))
                unit_activity(k, w);
            break;
          }
        }
    }
}

void reset_num_weather (void) {
  /*
     This recounts the amount of weather on the map -> a weather type thats double/triple/etc
     its normal weather type size consequently counts as 2/3/etc. weather occurences...
     Kinda wierd, a HUGE 4x size blizzard is equal to a semi-large 4x firestorm...
  */

  struct w_index *w=weather_index;
  if (!MAP_ACTIVE) return;
  num_weather=0;
  for (; w; w=w->next) {
    if (w->radius != weather_data[w->type][1])
      num_weather+=w->radius/weather_data[w->type][1];
    else
      num_weather++;
  }
}

void weather_activity (void) {
  int i=0;
  sh_int x, y;
  struct w_index *w=NULL, *lastw=NULL, *tw=NULL;

  if (!MAP_ACTIVE) return;

  for (w=weather_index; w;) {
    w->left--;

    if (w->left <= 0) {
      if (w==weather_index) {
        send_weather_messages(w, WEATHER_MSG_STOP);
        weather_index=weather_index->next;
        free(w);
        w=weather_index;
        reset_num_weather();
        continue;
      }
      send_weather_messages(w, WEATHER_MSG_STOP);
      lastw->next=w->next;
      free(w);
      w=lastw->next;
      reset_num_weather();
      continue;
    }

    // Update & destroy all weather that has finished or gone off the map - spawn new weather.

    x=rm2x(w->in_room);
    y=rm2y(w->in_room);

    if (weather_data[w->type][2] >= 0) // Make sure the weather doesn't do negative damage, we don't have healing storms do we? hehe
    switch (w->dir) {
      case NORTH:
        for (i=1; i<=weather_data[w->type][0]; i++) { w->in_room=find_room_by_coords(x,y-1); y--; radial_activity(w); } break;
      case SOUTH:
        for (i=1; i<=weather_data[w->type][0]; i++) { w->in_room=find_room_by_coords(x,y+1); y++; radial_activity(w); } break;
      case EAST:
        for (i=1; i<=weather_data[w->type][0]; i++) { w->in_room=find_room_by_coords(x+1,y); x++; radial_activity(w); } break;
      case WEST:
        for (i=1; i<=weather_data[w->type][0]; i++) { w->in_room=find_room_by_coords(x-1,y); x--; radial_activity(w); } break;
    }
    if (weather_data[w->type][0]==0) // If the weather is stationary:
      radial_activity(w);            // Do radial activity (since it isn't done above).
    // Do radial activity & update weather positions.

    if (number(1, 100) <= weather_data[w->type][3])
      w->dir = number(0, 3);
    // Randomly shift directions for weather...
    send_weather_messages(w, WEATHER_MSG_ACT);
    lastw=w;  
    w=w->next;
  }
  if (num_weather<MAX_WEATHER) // Respawn weather occurences, one at a time...
    tw=spawn_weather(rand_weather(), -1, number(map_start_room, top_of_world));
  check_weather_collisions();
  update_weather_map();
}

#define WEATHER_VISION_RADIUS_X 20
#define WEATHER_VISION_RADIUS_Y 8
#define FILLER_CHAR '+'

char *weatherchar (room_rnum rnum, room_rnum inroom) {
  char *tmp, *tmp2;
  int i;
  if (!ismap(rnum)) return " ";

  tmp=weather_map[rm2y(rnum)-1][rm2x(rnum)-1];
  tmp2=weather_map[rm2y(rnum)-1][(i=rm2x(rnum)-2) < 0 ? i+max_map_x : i];

  i=rm2x(inroom)-WEATHER_VISION_RADIUS_X; // Our threshhold.
  if (i < 1) i+=max_map_x;

  if (!strncmp(tmp, tmp2, 2) && inroom!=find_room_by_coords(rm2x(rnum)-1, rm2y(rnum)) &&
      tmp[0]=='&' && i!=rm2x(rnum))
    return tmp+2; // Print the character without the colorcode... complete genius! (Saves ALOTTA bandwidth)
  else
    return tmp;
}

void printweather(struct char_data * ch) {
  int j, k, x, y, inroom;
  char buf[2000];
  if (!MAP_ACTIVE) return;
  if (!ismap(ch->in_room)) {
    x=zone_table[world[ch->in_room].zone].ZWPointX;
    y=zone_table[world[ch->in_room].zone].ZWPointY;
  }
  else {
    x=rm2x(ch->in_room);
    y=rm2y(ch->in_room);
  }
  inroom=find_room_by_coords(x,y);
  strcpy(buf, MAP_INDENT"&y+&n Map of Deltania's Weather &y+&n\r\n");
  strcat(buf, "&n.&c");
  for (j=-WEATHER_VISION_RADIUS_X; j<=WEATHER_VISION_RADIUS_X; j++)
    strcat(buf, "-");
  strcat(buf, "&n.\r\n");
  send_to_char(buf, ch);

  for (j=-WEATHER_VISION_RADIUS_Y; j<=WEATHER_VISION_RADIUS_Y; j++) {
    sprintf(buf, "&c|");
    for (k=-WEATHER_VISION_RADIUS_X; k<=WEATHER_VISION_RADIUS_X; k++) {
      if (k==0 && j == 0) 
        strcat(buf, "&w#");
      else
        strcat(buf, weatherchar(find_room_by_coords(x+k, y+j), inroom));
    }
    strcat(buf, "&c| ");
    if (j+WEATHER_VISION_RADIUS_Y==0) sprintf(buf+strlen(buf), "&nDirections:");
    if (j+WEATHER_VISION_RADIUS_Y==1) sprintf(buf+strlen(buf), "&n%c = North %c = South", DIRECTION_NORTH, DIRECTION_SOUTH);
    if (j+WEATHER_VISION_RADIUS_Y==2) sprintf(buf+strlen(buf), "&n%c = East  %c = West", DIRECTION_EAST, DIRECTION_WEST);
    if (j+WEATHER_VISION_RADIUS_Y==3) sprintf(buf+strlen(buf), "&n%c = Stationary", DIRECTION_STATIONARY);
    if (j+WEATHER_VISION_RADIUS_Y==5) sprintf(buf+strlen(buf), "&nWeather:");
    if (j+WEATHER_VISION_RADIUS_Y>5)
      if (j+WEATHER_VISION_RADIUS_Y-6<WEATHER_TOTAL)
        sprintf(buf+strlen(buf), "%s%s = %s", weather_colors[j+WEATHER_VISION_RADIUS_Y-6], weather_chars[j+WEATHER_VISION_RADIUS_Y-6], weather_names[j+WEATHER_VISION_RADIUS_Y-6]);
    strcat(buf, "\r\n");
    send_to_char(buf, ch);
  }
  sprintf(buf, "&n`&c");

  for (j=-WEATHER_VISION_RADIUS_X; j<=WEATHER_VISION_RADIUS_X; j++)
    strcat(buf, "-");
  strcat(buf, "&n'\r\n");
  send_to_char(buf, ch);
}

void listweather(struct char_data *ch) {
  struct w_index *w;
  int i=1;

  if (map_start_room==-1) {
    send_to_char("Map not loaded.\r\n", ch);
    return;
  }

  for (w=weather_index; w; w=w->next) {
    sprintf(buf, "(%d) There is a %13s with a radius of %2d %s", i, weather_names[w->type], w->radius, weather_data[w->type][0] <= 0 ? "[stationary] " : "");
    if (weather_data[w->type][0]>0)
      switch (w->dir) {
        case NORTH: strcat(buf, "going  north "); break;
        case SOUTH: strcat(buf, "going  south "); break;
        case EAST: strcat(buf,  "going   east "); break;
        case WEST: strcat(buf,  "going   west "); break;
      }
    sprintf(buf+strlen(buf), "at %2dx%2d. Expiration in mins: %d.%d\r\n", rm2x(w->in_room), rm2y(w->in_room), w->left/2, w->left%2 == 0 ? 0 : 5);
    send_to_char(buf, ch);
    i++;
  }
  sprintf(buf, "Weather to be autospawned: %d\r\n", MAX(MAX_WEATHER-num_weather, 0));
  send_to_char(buf, ch);
}

int isinradius (int room, int room2, int radius) {
  return isinradius_bycoords(rm2x(room), rm2y(room), rm2x(room2), rm2y(room2), radius);
}

/*
int isinradius (int room, int room2, int radius) {
  int mn, ln, x, y, centerx, centery;
  if (!ismap(room) || !ismap(room2)) return 0;
  x=rm2x(room);
  y=rm2y(room);
  centerx=rm2x(room2);
  centery=rm2y(room2);

  mn=(radius*radius)-((x-centerx)*(x-centerx));
  Determine the first part of the circle equation, if this is negative return 0 (its not in the circle).

  if (mn<0) return 0;

  ln=sqrt(mn)+centery;

  Determine what the 'y' would be if the room's x were on the edge of the circle, if its less, the room is southeast of the circle and not in it.
  if (ln < y) return 0;

  We've now got to determine if the 'y' is inside the circle... simple, just subtract 2x the distance from the 'y' point to the center's y and compare...
  if (y < ln-(2*(ln-centery))) return 0;

  return 1;
}
*/

#define MARK(room) (SET_BIT(ROOM_FLAGS(room), ROOM_BFS_MARK))
#define UNMARK(room) (REMOVE_BIT(ROOM_FLAGS(room), ROOM_BFS_MARK))
#define IS_MARKED(room) (IS_SET(ROOM_FLAGS(room), ROOM_BFS_MARK))

void send_weather_messages (struct w_index *w, int type) {
  int radius=w->radius, x, y, rm;
  int j;
  room_rnum k;
  if (!MAP_ACTIVE) return;

  for (j=map_start_room; j<=top_of_world; j++) UNMARK(j);

  switch (type) {
    case WEATHER_MSG_FORM:
      sprintf(buf, "You see a %s brewing to your ", weather_names[w->type]);
      sprintf(buf2, "You see a %s brewing above you.\r\n", weather_names[w->type]);
      break;
    case WEATHER_MSG_ACT:
      sprintf(buf, "You see a %s to your ", weather_names[w->type]);
      sprintf(buf2, "%s\r\n", (char *)weather_messages[w->type]);
      break;
    case WEATHER_MSG_STOP:
      sprintf(buf, "You see a %s dissipate to your ", weather_names[w->type]);
      sprintf(buf2, "The %s above you dissipates.\r\n", weather_names[w->type]);
      break;
    default: return;
  }
  for (y=rm2y(w->in_room)-(2*radius); y<=rm2y(w->in_room)+(2*radius); y++)
    for (x=rm2x(w->in_room)-(2*radius); x<=rm2x(w->in_room)+(2*radius); x++) {
      rm=find_room_by_coords(x, y);
      if (((ismap(rm) && world[rm].people) || world[rm].wzonecontrol!=0) && !IS_MARKED(rm)) {
        MARK(rm);
        if (isinradius_wrap(rm2x(rm),rm2y(rm),rm2x(w->in_room),rm2y(w->in_room), radius))
          strcpy(buf1, buf2);
        else {
          strcpy(buf1, buf);
          if (y<rm2y(w->in_room))
            strcat(buf1, "south");
          if (y>rm2y(w->in_room))
            strcat(buf1, "north");
          if (x<rm2x(w->in_room))
            strcat(buf1, "east");
          if (x>rm2x(w->in_room))
            strcat(buf1, "west");
          strcat(buf1, ".\r\n");

        }
        if (!ROOM_FLAGGED(rm, ROOM_INDOORS))
          send_to_room(buf1, rm);
        if (world[rm].wzonecontrol!=0)
          for (j=0; j<=top_of_zone_table; j++) {
            if (zone_table[j].number == world[rm].wzonecontrol) {
              for (k=real_room(j*100); world[k].zone==j; k++)
                if (!ROOM_FLAGGED(k, ROOM_INDOORS))
                  send_to_room(buf1, k);
              break;
            }
          }
      }
    }
  // Send messages to all people outside of the weather and in it.
}

ACMD(lweather) {
  int i, j;
  struct w_index *w=weather_index;
/*  void biorythm(struct char_data *ch);
  biorythm(ch); */
  unsigned int k;
  get_arg(argument, 1, buf);
sscanf(argument, "%x", &k);
sprintf(buf, "%i\r\n", k);
send_to_char(buf, ch);
return;
  if (!strcasecmp(buf, "update_weather_activity")) {
    weather_activity();
    return;
  }
  if (!strcasecmp(buf, "update_weather_map")) {
    update_weather_map();
    return;
  }
  if (!strcasecmp(buf, "new")) {
    get_arg(argument, 2, buf);
    for (i=0; i<WEATHER_TOTAL; i++) {
      get_arg((char *)weather_names[i], 1, buf1);
      if (!strcasecmp(buf, buf1)) {
        get_arg(argument, 3, buf);
        j=cdsr(buf);
        w=spawn_weather(i, -1, j == -1 ? (j=number(map_start_room, top_of_world)) : j);
        sprintf(buf, "Spawned a %s at %s.\r\n", weather_names[i], rcds(j));
        send_to_char(buf, ch);
        update_weather_map();
        return;
      }
    }
  }
  if (!strcasecmp(buf, "destroy")) {
    get_arg(argument, 2, buf);
    if ((buf[0]=='0' && a2i(buf) == 0) || a2i(buf)>0) {
      sprintf(buf1, "Attempted to destroy meteoric occurence #%d.\r\n", a2i(buf));
      send_to_char(buf1, ch);
      destroy_weather(a2i(buf));
      update_weather_map();
      return;
    }
  }
  if (!strcasecmp(buf, "edit")) {
    get_arg(argument, 2, buf);
    for (i=1; i<atoi(buf); i++)
      w=w->next;
    if (w) {
    if (atoi(buf)>0) {
      get_arg(argument, 3, buf);
      get_arg(argument, 4, buf1);
      if (!strcasecmp(buf, "type")) {
        for (i=0; i<WEATHER_TOTAL; i++) {
          get_arg((char *)weather_names[i], 1, buf2);
          if (!strcasecmp(buf1, buf2)) {
            w->type=i;
            return;
          }
        }
      }
      if (!strcasecmp(buf, "radius")) {
        w->radius=MIN(atoi(buf1), (max_map_x+max_map_y)/2);
        return;
      }
      if (!strcasecmp(buf, "expiration")) {
        w->left=MIN(atoi(buf1), 500);
        return;
      }
      if (!strcasecmp(buf, "location")) {
        if (cdsr(buf1)!=-1) {
          w->in_room=cdsr(buf1);
          return;
        }
      }
    }
    }
  }
  listweather(ch);
}

ACMD(pweather) {
  if ((world[ch->in_room].weather == WEATHER_FOG || world[ch->in_room].weather == WEATHER_MAGICFOG) && (GET_LEVEL(ch)<LVL_IMMORT || IS_NPC(ch)))
    send_to_char("The thick fog prevents you from determining the weather.\r\n", ch);
  else if (ROOM_FLAGGED(ch->in_room, ROOM_INDOORS))
    send_to_char("Determine the weather indoors!? Impossible!\r\n", ch);
  else if (ismap(ch->in_room))
    printweather(ch);
  else if (zone_table[world[ch->in_room].zone].ZWPointX != 0 && zone_table[world[ch->in_room].zone].ZWPointY !=0)
    printweather(ch);
  else
    send_to_char("Notify immortals that this zone's ZWeatherPoint is unset please.\r\n", ch);
}

/* Weather map buffering routines ... */

void weather_alloc (void) {
  sh_int i;
  if (map_start_room == -1 || weather_map) return;

  weather_map=(char ***) calloc(max_map_y, sizeof(char *)); // First dimension of the array.
  for (i=0; i<=max_map_y-1; i++)
    weather_map[i]=(char **) calloc(max_map_x, sizeof(char *)); // Second dimension of the array.

  w_cchars=(char **) calloc((WEATHER_TOTAL*6)+1, sizeof(char *)); // Second dimension of the array. 
  sprintf(buf, "&n%c", FILLER_CHAR);
  w_cchars[0]=strdup(buf);
  for (i=1; i<=WEATHER_TOTAL*6; i+=6) {
    sprintf(buf, "%c%c", weather_colors[i/6][0], weather_colors[i/6][1]);
    buf[3]='\0';
    buf[2]=DIRECTION_NORTH;
    w_cchars[i]=strdup(buf);
    buf[2]=DIRECTION_SOUTH;
    w_cchars[i+1]=strdup(buf);
    buf[2]=DIRECTION_EAST;
    w_cchars[i+2]=strdup(buf);
    buf[2]=DIRECTION_WEST;
    w_cchars[i+3]=strdup(buf);
    buf[2]=DIRECTION_STATIONARY;
    w_cchars[i+4]=strdup(buf);
    buf[2]=weather_chars[i/6][0];
    w_cchars[i+5]=strdup(buf);
  }
}

void swc (sh_int x, sh_int y, char *a, int type) {
  int i, j, nx=x, ny=y;
  room_rnum k;
  WRAPX(nx);
  WRAPY(ny);
  nx--;
  ny--;
  weather_map[ny][nx]=a;
  if ((i=find_room_by_coords(nx+1,ny+1))!=-1) {
    world[i].weather=type;
    if (world[i].wzonecontrol!=0)
      for (j=0; j<=top_of_zone_table; j++) {
        if (zone_table[j].number == world[i].wzonecontrol) {
          for (k=real_room(j*100); world[k].zone==j; k++)
            if (!ROOM_FLAGGED(k, ROOM_INDOORS))
              world[k].weather=type;
          break;
        }
      }
  }
}

int isinradius_wrap (sh_int x1, sh_int y1, sh_int x2, sh_int y2, int radius) {
  if (isinradius_bycoords(x1, y1, x2, y2, radius)) return WRAP_NORM_NORM;
// Normal

  if (isinradius_bycoords(x1+max_map_x, y1, x2, y2, radius)) return WRAP_PLUS_NORM;
  if (isinradius_bycoords(x1, y1+max_map_y, x2, y2, radius)) return WRAP_NORM_PLUS;
  if (isinradius_bycoords(x1+max_map_x, y1+max_map_y, x2, y2, radius)) return WRAP_PLUS_PLUS;
// Additions

  if (isinradius_bycoords(x1-max_map_x, y1, x2, y2, radius)) return WRAP_LESS_NORM;
  if (isinradius_bycoords(x1, y1-max_map_y, x2, y2, radius)) return WRAP_NORM_LESS;
  if (isinradius_bycoords(x1-max_map_x, y1-max_map_y, x2, y2, radius)) return WRAP_LESS_LESS;
// Subtractions

  if (isinradius_bycoords(x1+max_map_x, y1-max_map_y, x2, y2, radius)) return WRAP_PLUS_LESS;
  if (isinradius_bycoords(x1-max_map_x, y1+max_map_y, x2, y2, radius)) return WRAP_LESS_PLUS;
// Both
  return 0;
}

int isinradius_bycoords (sh_int x1, sh_int y1, sh_int x2, sh_int y2, int radius) {
  if ((x1-x2)*(x1-x2) + (y1-y2)*(y1-y2) <= radius*radius)
    return 1;
  return 0;
}

/*
int isinradius_bycoords (sh_int x1, sh_int y1, sh_int x2, sh_int y2, int radius) {
  int mn, ln, x, y, centerx, centery;
  x=x1;
  y=y1;
  centerx=x2;
  centery=y2;

  if ((x-centerx)*(x-centerx) + (y-centery)*(y-centery) <= radius*radius) return 1; return 0;

  mn=(radius*radius)-((x-centerx)*(x-centerx));
  if (mn<0) return 0;
  ln=sqrt(mn)+centery;
  if (ln < y) return 0;
  if (y < ln-(2*(ln-centery))) return 0;
  return 1;
}
*/

void update_weather_map (void) {
  struct w_index *w;
  sh_int x, y;
  int radius;
  char *dirchar;

  if (map_start_room==-1) return;

  for (y=1; y<=max_map_y; y++)
    for (x=1; x<=max_map_x; x++)
      swc(x,y,w_cchars[0],WEATHER_NONE);

  for (w=weather_index; w; w=w->next) {
    radius=w->radius;
    if (weather_data[w->type][0]<=0)
      dirchar=w_cchars[w->type*6+5];
    else
      switch (w->dir) {
        case NORTH: dirchar=w_cchars[w->type*6+1]; break;
        case SOUTH: dirchar=w_cchars[w->type*6+2]; break;
        case EAST: dirchar=w_cchars[w->type*6+3]; break;
        case WEST: dirchar=w_cchars[w->type*6+4]; break;
        default: dirchar=w_cchars[w->type*6+5]; break;
      }
    for (y=rm2y(w->in_room)-radius; y<=rm2y(w->in_room)+radius; y++) {
      for (x=rm2x(w->in_room)-radius; x<=rm2x(w->in_room)+radius; x++) {
        if (isinradius_bycoords(x,y,rm2x(w->in_room),rm2y(w->in_room),radius)) {
          if ((x+y)%2!=0)
            swc(x,y,dirchar,w->type);
          else
            swc(x,y,w_cchars[w->type*6+6],w->type);
        }
      }
    }
  }
}

/*
void update_weather_map (void) {
  struct w_index *w;
  sh_int x, y, s, nx, ny;
  int radius;
  char dirchar;

  sprintf(buf, "&n%c", FILLER_CHAR);

  for (y=1; y<=max_map_y; y++)
    for (x=1; x<=max_map_x; x++)
      swc(x,y,buf,WEATHER_NONE);

  for (w=weather_index; w; w=w->next) {
    x=0;
    y=0;
    s=0;
    nx=0;
    ny=0;
    radius=0;
    radius=w->radius;
    if (weather_data[w->type][0]<=0)
      dirchar=DIRECTION_STATIONARY;
    else
      switch (w->dir) {
        case NORTH: dirchar=DIRECTION_NORTH; break;
        case SOUTH: dirchar=DIRECTION_SOUTH; break;
        case EAST: dirchar=DIRECTION_EAST; break;
        case WEST: dirchar=DIRECTION_WEST; break;
        default: dirchar=DIRECTION_STATIONARY; break;
      }
    sprintf(buf2, "%c%c", weather_colors[w->type][0], weather_colors[w->type][1]);
    buf2[3]='\0';
    nx=rm2x(w->in_room);
    ny=rm2y(w->in_room);
    while (radius > 0) {
      x = 0;
      y = radius;
      s = 3 - (2*radius);
      while(x<=y) {
        if ((x+y)%2!=0)
          buf2[2]=dirchar;
        else
          buf2[2]=weather_chars[w->type][0];
        swc(nx+x,ny+y,buf2,w->type);
        swc(nx+x,ny-y,buf2,w->type);
        swc(nx-x,ny+y,buf2,w->type);
        swc(nx-x,ny-y,buf2,w->type);
        swc(nx+y,ny+x,buf2,w->type);
        swc(nx+y,ny-x,buf2,w->type);
        swc(nx-y,ny+x,buf2,w->type);
        swc(nx-y,ny-x,buf2,w->type);
        if (s < 0)
          s += 4 * x + 6;
        else {
          s += 4 * (x - y) + 10;
          y--;
        }
        x++;
      }
      radius--;
    }
    if ((nx+ny)%2!=0)
      buf2[2]=dirchar;
    else
      buf2[2]=weather_chars[w->type][0];
    swc(nx, ny, buf2);
  }
}
*/

int weather_movement_increase (room_rnum in_room, room_rnum to_room, int need_movement) {
  need_movement+=
    (world[in_room].weather==WEATHER_RAINSTORM ? need_movement/12 : 0) +
    (world[to_room].weather==WEATHER_RAINSTORM ? need_movement/12 : 0);
  need_movement+=
    (world[in_room].weather==WEATHER_SNOWSTORM ? need_movement/10 : 0) +
    (world[to_room].weather==WEATHER_SNOWSTORM ? need_movement/10 : 0);
  need_movement+=
    (world[in_room].weather==WEATHER_THUNDERSTORM ? need_movement/12 : 0) +
    (world[to_room].weather==WEATHER_THUNDERSTORM ? need_movement/12 : 0);
  need_movement+=
    (world[in_room].weather==WEATHER_HURRICANE ? need_movement/9 : 0) +
    (world[to_room].weather==WEATHER_HURRICANE ? need_movement/9 : 0);
  need_movement+=
    (world[in_room].weather==WEATHER_TORNADO ? need_movement/9 : 0) +
    (world[to_room].weather==WEATHER_TORNADO ? need_movement/9 : 0);
  need_movement+=
    (world[in_room].weather==WEATHER_BLIZZARD ? need_movement/4 : 0) +
    (world[to_room].weather==WEATHER_BLIZZARD ? need_movement/4 : 0);
  return need_movement;
}
