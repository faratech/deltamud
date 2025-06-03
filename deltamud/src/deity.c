#include "sysdep.h"
#include "conf.h"

#include "structs.h"
#include "interpreter.h"
#include "utils.h"

const char *deity_abbrevs[] =
{
  "Aet",
  "Cor",
  "Lyt",
  "Pal",
  "Ero",
  "Cai",
  "Mar",
  "Pos",
  "Lel",
  "Cha",
  "Ele",
  "Nem",
  "Dur",
  "Ran",
  "Inc",
  "\n"
};

const char *pc_deity_types[] =
{
  "Aetos",
  "Corgus",
  "Lythern",
  "Pallas",
  "Eros",
  "Cailia",
  "Marbin",
  "Poseidon",
  "Lelu",
  "Chaos",
  "Elestra",
  "Nemonica",
  "Durn",
  "Ranus",
  "Incubus",
  "\n"
};

const char **deity_types = pc_deity_types;

/* The menu for choosing a deity in interpreter.c: */
const char *deity_menu =
"\r\n"
"&YSelect a deity to worship&n&R:&n\r\n"
"\r\n"
"    Good                Neutral               Evil&n\r\n"
"    &g-&y-&g-&y-                &g-&y-&g-&y-&g-&y-&g-               &g-&y-&g-&y-&n\r\n"
"  &c[&na&c]&n Aetos           &c[&nf&c]&n Cailia           &c[&nk&c]&n Elestra\r\n"
"  &c[&nb&c]&n Corgus          &c[&ng&c]&n Marbin           &c[&nl&c]&n Nemonica\r\n"
"  &c[&nc&c]&n Lythern         &c[&nh&c]&n Poseidon         &c[&nm&c]&n Durn\r\n"
"  &c[&nd&c]&n Pallas          &c[&ni&c]&n Lelu             &c[&nn&c]&n Ranus\r\n"
"  &c[&ne&c]&n Eros            &c[&nj&c]&n Chaos            &c[&no&c]&n Incubus\r\n";

int parse_deity (char arg)
{
  arg = LOWER (arg);  

  switch (arg)
    {
    case 'a':
      return DEITY_AETOS;
      break;
    case 'b':
      return DEITY_CORGUS;
      break;
    case 'c':
      return DEITY_LYTHERN;
      break;
    case 'd':
      return DEITY_PALLAS;
      break;
    case 'e':
      return DEITY_EROS;
      break;
    case 'f':
      return DEITY_CAILIA;
      break;
    case 'g':
      return DEITY_MARBIN;
      break;
    case 'h':
      return DEITY_POSEIDON;
      break;
    case 'i':
      return DEITY_LELU;
      break;
    case 'j':
      return DEITY_CHAOS;
      break;
    case 'k':
      return DEITY_ELESTRA;
      break;
    case 'l':
      return DEITY_NEMONICA;
      break;
   case 'm':
      return DEITY_DURN;
      break;
    case 'n':
      return DEITY_RANUS;
      break;
    case 'o':
      return DEITY_INCUBUS;
      break;
    default:
      return DEITY_UNDEFINED;
      break; 
    }
}

long
find_deity_bitvector (char arg)
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
    case 'j':
      return (1 << 9);
      break;
    case 'k':
      return (1 << 10);
      break;
    case 'l':
      return (1 << 11);
      break;
    case 'm':
      return (1 << 12);
      break;
    case 'n':
      return (1 << 13);
      break;
    case 'o':
      return (1 << 14);
      break;
    default:
      return 0;
      break; 
    }
}
