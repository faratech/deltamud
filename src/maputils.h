/*****************************************************
 * maputils.h --- implementation file for ascii maps *
 *****************************************************/

#ifndef __MAPUTILS_C__
  extern struct s_index *sect_index;
  extern sh_int max_map_x;
  extern sh_int max_map_y;
  extern sh_int map_start_room;
#endif

struct s_index {
  char id;
  char *show;
  char *name;
  int move;
  int sect;
  char *desc;
  struct s_index *next;
};

struct w_index {
  int type;
  room_rnum in_room;
  sh_int left;
  int dir;
  sh_int radius;
  struct w_index *next;
};

#define MAP_ACTIVE (map_start_room != -1 && max_map_x && max_map_y && sect_index)
#define WEATHER_ACTIVE (weather_index)

int wrap_method_x (int x, int method);
int wrap_method_y (int y, int method);
int isinradius_wrap (sh_int x1, sh_int y1, sh_int x2, sh_int y2, int radius);
int isinradius_bycoords (sh_int x1, sh_int y1, sh_int x2, sh_int y2, int radius);
char *rcds(room_rnum room);
void unload_map(void);
void read_map(void);
room_rnum find_room_by_coords(sh_int x, sh_int y);
int ismap(room_rnum room);
sh_int rm2x(room_rnum room);
sh_int rm2y(room_rnum room);
void buildmapexits(void);
void get_arg(char *string, int argnum, char *arg);
void get_arg_exclude(char *string, int argnum, char *arg);
int count_args(char *string);
int compare(char *string1, char *string2);
void printmap(int rnum, struct char_data * ch);
char *check_noroom(room_rnum i, struct char_data *ch, int radius, int modifier);
struct s_index *find_sect_by_id(char id);
char lowcase(char a);
room_rnum newroom(void);
void clear_room(room_rnum room);
room_rnum cdsr(char *string);
int weather_movement_increase (room_rnum in_room, room_rnum to_room, int need_movement);

#define WEATHER_NONE		-1
#define WEATHER_RAINSTORM	0
#define WEATHER_SNOWSTORM	1
#define WEATHER_THUNDERSTORM	2
#define WEATHER_FIRESTORM	3
#define WEATHER_FOG		4
#define WEATHER_MAGICFOG	5
#define WEATHER_HURRICANE	6
#define WEATHER_TORNADO		7
#define WEATHER_BLIZZARD	8
#define WEATHER_DEATH		9

#define WEATHER_TOTAL	10

#define AVOID_WEATHER(x) ( x == WEATHER_THUNDERSTORM || x == WEATHER_FIRESTORM || x == WEATHER_MAGICFOG || x == WEATHER_HURRICANE || x == WEATHER_TORNADO || x == WEATHER_BLIZZARD || x == WEATHER_DEATH )

#define DIRECTION_NORTH	'^'
#define DIRECTION_SOUTH	'v'
/*

#define DIRECTION_WEST	'«'
#define DIRECTION_EAST	'»'

 */
#define DIRECTION_WEST	'<'
#define DIRECTION_EAST	'>'
#define DIRECTION_STATIONARY '-'
#define FILLER_CHAR '+' // Character thats in the space where there is no weather.

#define MAX_WEATHER 4

/* Types of ways to wrap the map... */
#define WRAP_NORM_NORM 1
#define WRAP_NORM_LESS 2
#define WRAP_NORM_PLUS 3
#define WRAP_PLUS_NORM 4
#define WRAP_PLUS_LESS 5
#define WRAP_PLUS_PLUS 6
#define WRAP_LESS_NORM 7
#define WRAP_LESS_LESS 8
#define WRAP_LESS_PLUS 9

/* Types of weather messages to send... */
#define WEATHER_MSG_FORM 1
#define WEATHER_MSG_ACT  2
#define WEATHER_MSG_STOP 3
