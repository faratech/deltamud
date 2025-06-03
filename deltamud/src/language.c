/***************************************************************************
 *  Original Diku Mud copyright (C) 1990, 1991 by Sebastian Hammer,        *
 *  Michael Seifert, Hans Henrik St{rfeldt, Tom Madsen, and Katja Nyboe.   *
 *                                                                         *
 *  Merc Diku Mud improvments copyright (C) 1992, 1993 by Michael          *
 *  Chastain, Michael Quan, and Mitchell Tse.                              *
 *                                                                         *
 *  Envy Diku Mud improvements copyright (C) 1994 by Michael Quan, David   *
 *  Love, Guilherme 'Willie' Arnold, and Mitchell Tse.                     *
 *                                                                         *
 *  EnvyMud 2.0 improvements copyright (C) 1995 by Michael Quan and        *
 *  Mitchell Tse.                                                          *
 *                                                                         *
 *  In order to use any part of this Envy Diku Mud, you must comply with   *
 *  the original Diku license in 'license.doc', the Merc license in        *
 *  'license.txt', as well as the Envy license in 'license.nvy'.           *
 *  In particular, you may not remove either of these copyright notices.   *
 *                                                                         *
 *  Much time and thought has gone into this software and you are          *
 *  benefitting.  We hope that you share your changes too.  What goes      *
 *  around, comes around.                                                  *
 ***************************************************************************/
#include "conf.h"
#include "sysdep.h"

#include "structs.h"
#include "utils.h"
#include "comm.h"
#include "interpreter.h"
#include "handler.h"
#include "db.h"
#include "screen.h"

/*
 * Drunk struct
 */
struct drunk_struct
{
        int     min_drunk_level;
        int     number_of_rep;
        char    *replacement[11];
};

char    *makedrunk(char *string ,struct char_data *ch);

/* How to make a string look drunk... by Apex (robink@htsa.hva.nl) */
/* Modified and enhanced for envy(2) by the Maniac from Mythran    */
/* Ported to Stock Circle 3.0 by Haddixx (haddixx@megamed.com)     */

char * makedrunk (char *string, struct char_data * ch)
{

/* This structure defines all changes for a character */
  struct drunk_struct drunk[] =
  {
    {3, 10,
      {"a", "a", "a", "A", "aa", "ah", "Ah", "ao", "aw", "oa", "ahhhh"}},
    {8, 5,
     {"b", "b", "b", "B", "B", "vb"}},
    {3, 5,
     {"c", "c", "C", "cj", "sj", "zj"}},
    {5, 2,
     {"d", "d", "D"}},
    {3, 3,
     {"e", "e", "eh", "E"}},
    {4, 5,
     {"f", "f", "ff", "fff", "fFf", "F"}},
    {8, 2,
     {"g", "g", "G"}},
    {9, 6,
     {"h", "h", "hh", "hhh", "Hhh", "HhH", "H"}},
    {7, 6,
     {"i", "i", "Iii", "ii", "iI", "Ii", "I"}},
    {9, 5,
     {"j", "j", "jj", "Jj", "jJ", "J"}},
    {7, 2,
     {"k", "k", "K"}},
    {3, 2,
     {"l", "l", "L"}},
    {5, 8,
     {"m", "m", "mm", "mmm", "mmmm", "mmmmm", "MmM", "mM", "M"}},
    {6, 6,
     {"n", "n", "nn", "Nn", "nnn", "nNn", "N"}},
    {3, 6,
     {"o", "o", "ooo", "ao", "aOoo", "Ooo", "ooOo"}},
    {3, 2,
     {"p", "p", "P"}},
    {5, 5,
     {"q", "q", "Q", "ku", "ququ", "kukeleku"}},
    {4, 2,
     {"r", "r", "R"}},
    {2, 5,
     {"s", "ss", "zzZzssZ", "ZSssS", "sSzzsss", "sSss"}},
    {5, 2,
     {"t", "t", "T"}},
    {3, 6,
     {"u", "u", "uh", "Uh", "Uhuhhuh", "uhU", "uhhu"}},
    {4, 2,
     {"v", "v", "V"}},
    {4, 2,
     {"w", "w", "W"}},
    {5, 6,
     {"x", "x", "X", "ks", "iks", "kz", "xz"}},
    {3, 2,
     {"y", "y", "Y"}},
    {2, 9,
     {"z", "z", "ZzzZz", "Zzz", "Zsszzsz", "szz", "sZZz", "ZSz", "zZ", "Z"}}
  };

  char buf[1024];      /* this should be enough (?) */
  char temp;
  int pos = 0;
  int randomnum;

  if (GET_COND(ch, DRUNK) > 0)  /* character is drunk */
  {
     do
     {
       temp = toupper(*string);
       if( (temp >= 'A') && (temp <= 'Z') )
       {
         if(GET_COND(ch, DRUNK) > drunk[(temp - 'A')].min_drunk_level)
         {
           randomnum = number(0, (drunk[(temp - 'A')].number_of_rep));
           strcpy(&buf[pos], drunk[(temp - 'A')].replacement[randomnum]);
           pos += strlen(drunk[(temp - 'A')].replacement[randomnum]);
         }
         else
           buf[pos++] = *string;
       }
       else
       {
         if ((temp >= '0') && (temp <= '9'))
         {
           temp = '0' + number(0, 9);
           buf[pos++] = temp;
         }
         else
           buf[pos++] = *string;
       }
     }while (*string++);
     
     buf[pos] = '\0';          /* Mark end of the string... */
     strcpy(string, buf);
     return(string);
  }
  return (string); /* character is not drunk, just return the string */
}  
