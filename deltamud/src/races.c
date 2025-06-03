#include "sysdep.h"
#include "conf.h"

#include "structs.h"
#include "interpreter.h"
#include "utils.h"

const char *race_abbrevs[] =
{
  "Hum",
  "Elf",
  "Gno",
  "Dwa",
  "Tro",
  "Gob",
  "Dro",
  "Orc",
  "Min",
  "\n"
};

const char *pc_race_types[] =
{
  "Human",
  "Elf",
  "Gnome",
  "Dwarf",
  "Troll",
  "Goblin",
  "Drow",
  "Orc",
  "Minotaur",
  "\n"
};

const char **race_types = pc_race_types;

/* The menu for choosing a race in interpreter.c: */
const char *race_menu =
"\r\n"
"&YSelect a race&n&R:&n\r\n"
"  &c[&na&c]&n Human    - average creatures of diplomacy, ethics, and adventure\r\n"
"  &c[&nb&c]&n Elf      - smart, dexterious creatures, lacking strength & endurance\r\n"
"  &c[&nc&c]&n Gnome    - highly intelligent creatures who remain without wisdom\r\n"
"  &c[&nd&c]&n Dwarf    - healthy creatures who tend to remain isolated from others\r\n"
"  &c[&ne&c]&n Troll    - large masters of combat who lack intelligence and speed\r\n"
"  &c[&nf&c]&n Goblin   - grotesque, evil gnomes, well-known for their trickery\r\n"
"  &c[&ng&c]&n Drow     - strange, innovative people originating from another land\r\n"
"  &c[&nh&c]&n Orc      - rabbid wild creatures who take whatever they want\r\n"
"  &c[&ni&c]&n Minotaur - half human, half bull monsters who thrive on flesh\r\n";

/*
 * The code to interpret a race letter (used in interpreter.c when a 
 * new character is selecting a race).                               
 */
int 
parse_race (char arg)
{
  arg = LOWER (arg);

  switch (arg)
    {
    case 'a':
      return RACE_HUMAN;
      break;
    case 'b':
      return RACE_ELF;
      break;
    case 'c':
      return RACE_GNOME;
      break;
    case 'd':
      return RACE_DWARF;
      break;
    case 'e':
      return RACE_TROLL;
      break;
    case 'f':
      return RACE_GOBLIN;
      break;
    case 'g':
      return RACE_DROW;
      break;
    case 'h':
      return RACE_ORC;
      break;
    case 'i':
      return RACE_MINOTAUR;
      break;
    default:
      return RACE_UNDEFINED;
      break;
    }
}

long 
find_race_bitvector (char arg)
{
  arg = LOWER (arg);

  switch (arg)
    {
    case 'a':
      return (1 << 0);
      break;
    case 'b':
      return (1 << 1);
      break;
    case 'c':
      return (1 << 2);
      break;
    case 'd':
      return (1 << 3);
      break;
    case 'e':
      return (1 << 4);
      break;
    case 'f':
      return (1 << 5);
      break;
    case 'g':
      return (1 << 6);
      break;
    case 'h':
      return (1 << 7);
      break;
    case 'i': 
      return (1 << 8);
      break;  
    default:
      return 0;
      break;
    }
}
