/* ************************************************************************
   *   File: spec_assign.c                                 Part of CircleMUD *
   *  Usage: Functions to assign function pointers to objs/mobs/rooms        *
   *                                                                         *
   *  All rights reserved.  See license.doc for complete information.        *
   *                                                                         *
   *  Copyright (C) 1993, 94 by the Trustees of the Johns Hopkins University *
   *  CircleMUD is based on DikuMUD, Copyright (C) 1990, 1991.               *
   ************************************************************************ */

#include "conf.h"
#include "sysdep.h"

#include "structs.h"
#include "db.h"
#include "interpreter.h"
#include "utils.h"

extern struct room_data *world;
extern int top_of_world;
extern int mini_mud;
extern struct index_data *mob_index;
extern struct index_data *obj_index;

/* functions to perform assignments */

void 
ASSIGNMOB (int mob, SPECIAL (fname))
{
  if (real_mobile (mob) >= 0)
    mob_index[real_mobile (mob)].func = fname;
  else if (!mini_mud)
    {
      sprintf (buf, "SYSERR: Attempt to assign spec to non-existant mob #%d",
	       mob);
      log (buf);
    }
}

void 
ASSIGNOBJ (int obj, SPECIAL (fname))
{
  if (real_object (obj) >= 0)
    obj_index[real_object (obj)].func = fname;
  else if (!mini_mud)
    {
      sprintf (buf, "SYSERR: Attempt to assign spec to non-existant obj #%d",
	       obj);
      log (buf);
    }
}

void 
ASSIGNROOM (int room, SPECIAL (fname))
{
  if (real_room (room) >= 0)
    world[real_room (room)].func = fname;
  else if (!mini_mud)
    {
      sprintf (buf, "SYSERR: Attempt to assign spec to non-existant rm. #%d",
	       room);
      log (buf);
    }
}


/* ********************************************************************
   *  Assignments                                                        *
   ******************************************************************** */

/* assign special procedures to mobiles */
void 
assign_mobiles (void)
{
  SPECIAL (postmaster);
  SPECIAL (cityguard);
  SPECIAL (receptionist);
  SPECIAL (cryogenicist);
  SPECIAL (guild_guard);
  SPECIAL (guild);
  SPECIAL (puff);
  SPECIAL (fido);
  SPECIAL (janitor);
  SPECIAL (mayor);
  SPECIAL (snake);
  SPECIAL (thief);
  SPECIAL (magic_user);
  SPECIAL (librarian);
  SPECIAL (temple_cleric);
  SPECIAL (temple_healer);
  SPECIAL (temple_mana_regenerator);
  SPECIAL (generic_citizen);
  SPECIAL (trainer);
  SPECIAL (arenaentrancemaster);

  /* Limbo */
  ASSIGNMOB (1, puff);

  /* Immortal Zone */
  ASSIGNMOB (1200, receptionist);

  ASSIGNMOB (1201, postmaster);
  ASSIGNMOB (1202, janitor);

  /* Battle Arena */
  ASSIGNMOB (4800, arenaentrancemaster);        /* Thargor 7/25/98 */
  ASSIGNMOB (4801, temple_healer);	        /* Thargor 7/25/98 */
  ASSIGNMOB (4802, temple_mana_regenerator);	/* Thargor 7/28/98 */

  /* Itrius */
  ASSIGNMOB (199, postmaster);
  ASSIGNMOB (101, receptionist);
  ASSIGNMOB (102, librarian);
}



/* assign special procedures to objects */
void 
assign_objects (void)
{
  SPECIAL (bank);
  SPECIAL (gen_board);
  SPECIAL (portal);
  SPECIAL (tent);

  ASSIGNOBJ (20, portal); /* generic portal */
  ASSIGNOBJ (500, tent);

           /* mortal boards go below */ 
  ASSIGNOBJ (199, gen_board);  /* general board */
  ASSIGNOBJ (198, gen_board);  /* social board */
  ASSIGNOBJ (197, gen_board);  /* auction board */

           /* immortal boards go below */
  ASSIGNOBJ (1200, gen_board);  /* implementor board */
  ASSIGNOBJ (1201, gen_board);  /* general board */
  ASSIGNOBJ (1202, gen_board);  /* ideas board */
  ASSIGNOBJ (1203, gen_board);  /* quest board */
  ASSIGNOBJ (1204, gen_board);  /* relations board */
  ASSIGNOBJ (1205, gen_board);  /* frozen board */
  ASSIGNOBJ (1206, gen_board);  /* typos board */
  ASSIGNOBJ (1207, gen_board);  /* bugs board */
  ASSIGNOBJ (1208, gen_board);  /* reimbursement board */
  ASSIGNOBJ (1209, gen_board);  /* builders  board */
           /* clan boards go below */
}



/* assign special procedures to rooms */
void 
assign_rooms (void)
{
  extern int dts_are_dumps;
  int i;

  SPECIAL (dump);
  SPECIAL (pet_shops);
  SPECIAL (pray_for_items);

  if (dts_are_dumps)
    for (i = 0; i < top_of_world; i++)
      if (IS_SET (ROOM_FLAGS (i), ROOM_DEATH))
	world[i].func = dump;
}
