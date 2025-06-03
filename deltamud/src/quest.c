/***************************************************************************
 *  Original Diku Mud copyright (C) 1990, 1991 by Sebastian Hammer,        *
 *  Michael Seifert, Hans Henrik St{rfeldt, Tom Madsen, and Katja Nyboe.   *
 *                                                                         *
 *  Merc Diku Mud improvments copyright (C) 1992, 1993 by Michael          *
 *  Chastain, Michael Quan, and Mitchell Tse.                              *
 *                                                                         *
 *  In order to use any part of this Merc Diku Mud, you must comply with   *
 *  both the original Diku license in 'license.doc' as well the Merc       *
 *  license in 'license.txt'.  In particular, you may not remove either of *
 *  these copyright notices.                                               *
 *                                                                         *
 *  Much time and thought has gone into this software and you are          *
 *  benefitting.  We hope that you share your changes too.  What goes      *
 *  around, comes around.                                                  *
 ***************************************************************************/

/***************************************************************************
*       ROM 2.4 is copyright 1993-1995 Russ Taylor                         *
*       ROM has been brought to you by the ROM consortium                  *
*           Russ Taylor (rtaylor@pacinfo.com)                              *
*           Gabrielle Taylor (gtaylor@pacinfo.com)                         *
*           Brian Moore (rom@rom.efn.org)                                  *
*       By using this code, you have agreed to follow the terms of the     *
*       ROM license, in the file Rom24/doc/rom.license                     *
***************************************************************************/

/***************************************************************************
*  Automated Quest code written by Vassago of MOONGATE, moongate.ams.com   *
*  4000. Copyright (c) 1996 Ryan Addams, All Rights Reserved. Use of this  * 
*  code is allowed provided you add a credit line to the effect of:        *
*  "Quest Code (c) 1996 Ryan Addams" to your logon screen with the rest    *
*  of the standard diku/rom credits. If you use this or a modified version *
*  of this code, let me know via email: moongate@moongate.ams.com. Further *
*  updates will be posted to the rom mailing list. If you'd like to get    *
*  the latest version of quest.c, please send a request to the above add-  *
*  ress. Quest Code v2.00.                                                 *
***************************************************************************/
/*
 *  Modified to work with Circle 3.0 beta 11 Winter of 97 by DSW
 *  Some changes by: StormeRider <silk@ICI.NET>
 */

#include "conf.h"
#include "sysdep.h"
//#include <stdio.h>
//#include <stdlib.h>
//#include <string.h>
//#include <time.h>
#include "structs.h"
#include "utils.h"
#include "handler.h"
#include "interpreter.h"
#include "comm.h"
#include "db.h"

extern struct index_data *mob_index;
extern struct index_data *obj_index;
extern struct char_data *character_list;
extern struct object_data *object_list;
extern struct object_data *obj_proto;
extern struct char_data *mob_proto;
extern struct zone_data *zone_table;
extern int top_of_mobt;
extern int top_of_world;
extern struct room_data *world;

int real_mobile(int virtual);
int estimate_difficulty(struct char_data *ch, struct char_data *victim);

#define IS_QUESTOR(ch)     (PLR_FLAGGED(ch, PLR_QUESTOR))

/* Object vnums for Quest Rewards */
/* on Reimsmud just use item vnums in the godroom range: 1200's */
#define QUEST_ITEM1 9002
#define QUEST_ITEM2 9003
#define QUEST_ITEM3 3082
#define QUEST_ITEM4 3163
#define QUEST_ITEM5 8620
#define QUEST_ITEM6 3161
#define QUEST_ITEM7 3160
#define QUEST_ITEM8 9000
#define QUEST_ITEM9 9001
#define QUEST_ITEM10 6814
/* items 11 to 13 are sessions, gold, etc */
#define QUEST_ITEM14 18702
#define QUEST_ITEM15 9010

/* Object vnums for object quest 'tokens'. In Moongate, the tokens are
   things like 'the Shield of Moongate', 'the Sceptre of Moongate'. These
   items are worthless and have the rot-death flag, as they are placed
   into the world when a player receives an object quest. */

#define QUEST_OBJQUEST1 9005
#define QUEST_OBJQUEST2 9006
#define QUEST_OBJQUEST3 9007
#define QUEST_OBJQUEST4 9008
#define QUEST_OBJQUEST5 9009

/* Local functions */

void generate_quest(struct char_data *ch, struct char_data *questman);
void quest_update(void);
int qchance(int num);
ACMD(do_tell);

struct obj_data *create_object(int vnum, int dummy)
{
int r_num;
struct obj_data *tobj;

if((r_num = real_object(vnum)) < 0)
tobj = NULL;
else
 tobj = read_object(r_num, REAL);
 return(tobj);
}


/* QCHANCE function. I use this everywhere in my code, very handy :> */

int qchance(int num)
{
    if (number(1,100) <= num) return 1;
    else return 0;
}

/* The main quest function */

ACMD(do_autoquest)
{   char arg2[MAX_STRING_LENGTH], arg1[MAX_STRING_LENGTH];
    struct char_data *questman= NULL;
    struct obj_data *obj=NULL, *obj_next = NULL;
    struct obj_data *questinfoobj = NULL;
    struct char_data *questinfo = NULL;
    char mybuf[MAX_STRING_LENGTH];
    int cmd_tell = find_command("tell");

    two_arguments(argument, arg1, arg2);
    if (!strcmp(arg1, "info"))
    {
	if (IS_QUESTOR(ch))
	{
	    if ((GET_QUESTMOB(ch) < 0) &&
                ((questman = GET_QUESTGIVER(ch))!= NULL))
	    {
		sprintf(buf, "Your quest is ALMOST complete!\r\nGet back to %s before your time runs out!\r\n",
                     GET_NAME(GET_QUESTGIVER(ch)));
		send_to_char(buf, ch);
	    }
	    else if (GET_QUESTOBJ(ch) > 0)
	    {
                questinfoobj = get_obj_num(real_object(GET_QUESTOBJ(ch)));
		if (questinfoobj != NULL)
		{
		    sprintf(buf, "You are on a quest to recover the fabled %s!\r\n",questinfoobj->name);
		    send_to_char(buf, ch);
		}
		else send_to_char("You aren't currently on a quest.\r\n",ch);
               return;
	    }
	    else if (GET_QUESTMOB(ch) > 0)
	    {
                questinfo = get_char_num(real_mobile(GET_QUESTMOB(ch)));
		if (questinfo != NULL)
		{
	            sprintf(buf, "You are on a quest to slay the dreaded %s!\r\n",
                         GET_NAME(questinfo));
		    send_to_char(buf, ch);
		}
		else send_to_char("You aren't currently on a quest.\r\n",ch);
		return;
	    }
	}
	else
	    send_to_char("You aren't currently on a quest.\r\n",ch);
         return;
    }
    if (!strcmp(arg1, "points"))
    {
	sprintf(buf, "You have %d quest points.\r\n",GET_QUESTPOINTS(ch));
	send_to_char(buf, ch);
        return;
    }
    else if (!strcmp(arg1, "time"))
    {
	if (!IS_QUESTOR(ch))
	{
	    send_to_char("You aren't currently on a quest.\r\n",ch);
	    if (GET_NEXTQUEST(ch) > 1)
	    {
		sprintf(buf, "There are %d minutes remaining until you can go on another quest.\r\n",
                  GET_NEXTQUEST(ch));
		send_to_char(buf, ch);
	    }
	    else if (GET_NEXTQUEST(ch) == 1)
	    {
		sprintf(buf, "There is less than a minute remaining until you can go on another quest.\r\n");
		send_to_char(buf, ch);
	    }
	}
        else if (GET_COUNTDOWN(ch) > 0)
        {
	    sprintf(buf, "Time left for current quest is approximately %d minutes\r\n",
                 GET_COUNTDOWN(ch));
	    send_to_char(buf, ch);
	}
        return;
    }

    /* checks for a mob flagged MOB_QUESTMASTER */
    for ( questman = world[ch->in_room].people; questman ;
                        questman = questman->next_in_room )
    {
	if (!IS_NPC(questman)) continue;
        if (MOB_FLAGGED(questman, MOB_QUESTMASTER))
           break;
    }

    if (questman == NULL)
    {
        send_to_char("You can't do that here.\r\n",ch);
       return;
    }

    if ( FIGHTING(questman) != NULL)
    {
	send_to_char("Wait until the fighting stops.\r\n",ch);
        return;
    }

    GET_QUESTGIVER(ch) = questman;

/* And, of course, you will need to change the following lines for YOUR
   quest item information. Quest items on Moongate are unbalanced, very
   very nice items, and no one has one yet, because it takes awhile to
   build up quest points :> Make the item worth their while. */

    if (!strcmp(arg1, "list"))
    {
        act( "$n asks $N for a list of quest items.",FALSE,
               ch, NULL, questman, TO_ROOM); 
	act ("You ask $N for a list of quest items.",FALSE,
               ch, NULL, questman, TO_CHAR);
	sprintf(buf, "Current Quest Items available for Purchase:\r\n\r\n"
		/*  1 */ " #1 - 500,000..........Sword of the Gods\r\n"
		/*  2 */ " #2 - 30,000qp.........Midgaard Hero Vest\r\n"
		/*  3 */ " #3 - 25,000qp.........Divine Bag of Holding\r\n"
		/*  4 */ " #4 - 20,000qp.........Necklace of Recall     \r\n"
		/*  5 */ " #5 - 17,000qp.........Pendant of Age \r\n"
		/*  6 */ " #6 - 15,000qp.........Hat of Eternal Nourishment \r\n"
		/*  7 */ " #7 - 10,000qp.........Belt of Invisibility\r\n"
		/*  8 */ " #8 - 5,0000qp.........Black-eyed Ringstone \r\n"
		/*  9 */ " #9 - 4,000qp..........Black-eyed Infinite Loop \r\n"
		/* 10 */ "#10 - 3,000qp..........A golden claw\r\n"
		/* 11 */ "#11 - 2,000qp..........2,000,000 gold pieces\r\n"
		/* 12 */ "#12 - 1,500qp..........1 Training Session\r\n"
		/* 13 */ "#13 - 1,000qp..........30 Practice Sessions\r\n"
		/* 14 */ "#14 - 800qp............Boots of Water Walking \r\n"
		/* 15 */ "#15 - 500qp............A gold brick worth 50,000\r\n"
		"\r\nTo buy an item, type 'autoquest buy <#item>'.\r\n");
	send_to_char(buf, ch);
        return;
    }

    else if (!strcmp(arg1, "buy"))
    {
	if (arg2[0] == '\0')
	{
	    send_to_char("To buy an item, type 'autoquest buy <item>'.\r\n",ch);
            return;
	}
	if (isname(arg2, "#1 sword gods"))
	{
	    if (GET_QUESTPOINTS(ch) >= 500000)
	    {
		GET_QUESTPOINTS(ch) -= 500000;
	        obj = create_object(QUEST_ITEM1,GET_LEVEL(ch));
	    }
	    else
	    {
		sprintf(mybuf, "%s Sorry, but you don't have enough quest points for that.",GET_NAME(ch));
		do_tell(questman, mybuf, cmd_tell, 0);
		//act(buf, FALSE, questman, 0, 0, TO_ROOM);
		//send_to_char(buf, ch);
              return;
	    }
	}
	else if (isname(arg2, "#2 midgaard hero vest"))
	{
	    if (GET_QUESTPOINTS(ch) >= 30000)
	    {
		GET_QUESTPOINTS(ch) -= 30000;
	        obj = create_object(QUEST_ITEM2,GET_LEVEL(ch));
	    }
	    else
	    {
		sprintf(mybuf, "%s Sorry, but you don't have enough quest points for that.",GET_NAME(ch));
		do_tell(questman, mybuf, cmd_tell, 0);
                //send_to_char(buf, ch);
		//act(buf, FALSE, questman, 0, 0, TO_ROOM);
		return;
	    }
	}
	else if (isname(arg2, "#3 divine bag holding"))
	{
	    if (GET_QUESTPOINTS(ch) >= 25000)
	    {
                GET_QUESTPOINTS(ch) -= 25000;
	        obj = create_object(QUEST_ITEM3,GET_LEVEL(ch));
	    }
	    else
	    {
		sprintf(mybuf, "%s Sorry, but you don't have enough quest points for that.",GET_NAME(ch));
		do_tell(questman, mybuf, cmd_tell, 0);
                //send_to_char(buf, ch);
                //act(buf, FALSE, questman, 0, 0, TO_ROOM);
		return;
	    }
	}
	else if (isname(arg2, "#4 necklace recall"))
	{
	    if (GET_QUESTPOINTS(ch) >= 20000)
	    {
                GET_QUESTPOINTS(ch) -= 20000;
	        obj = create_object(QUEST_ITEM4,GET_LEVEL(ch));
	    }
	    else
	    {
		sprintf(mybuf, "%s Sorry, but you don't have enough quest points for that.",GET_NAME(ch));
		do_tell(questman, mybuf, cmd_tell, 0);
		return;
	    }
	}
	else if (isname(arg2, "#5 pendant age"))
	{
	    if (GET_QUESTPOINTS(ch) >= 17000)
	    {
		GET_QUESTPOINTS(ch) -= 17000;
	        obj = create_object(QUEST_ITEM5,GET_LEVEL(ch));
	    }
	    else
	    {
		sprintf(mybuf, "%s Sorry, but you don't have enough quest points for that.",GET_NAME(ch));
		do_tell(questman, mybuf, cmd_tell, 0);
                //send_to_char(buf, ch);
                //act(buf, FALSE, questman, 0, 0, TO_ROOM);
		return;
	    }
	}
	else if (isname(arg2, "#6 hat nourish nourishment eternal"))
	{
	    if (GET_QUESTPOINTS(ch) >= 15000)
	    {
		GET_QUESTPOINTS(ch) -= 15000;
	        obj = create_object(QUEST_ITEM6,GET_LEVEL(ch));
	    }
	    else
	    {
		sprintf(mybuf, "%s Sorry, but you don't have enough quest points for that.",GET_NAME(ch));
		do_tell(questman, mybuf, cmd_tell, 0);
                //send_to_char(buf, ch);
                //act(buf, FALSE, questman, 0, 0, TO_ROOM);
		return;
	    }
	}
	else if (isname(arg2, "#7 belt invis invisbility"))
	{
	    if (GET_QUESTPOINTS(ch) >= 10000)
	    {
		GET_QUESTPOINTS(ch) -= 10000;
	        obj = create_object(QUEST_ITEM7,GET_LEVEL(ch));
	    }
	    else
	    {
		sprintf(mybuf, "%s Sorry, but you don't have enough quest points for that.",GET_NAME(ch));
		do_tell(questman, mybuf, cmd_tell, 0);
                //send_to_char(buf, ch);
                //act(buf, FALSE, questman, 0, 0, TO_ROOM);
		return;
	    }
	}
	else if (isname(arg2, "#8 ringstone"))
	{
	    if (GET_QUESTPOINTS(ch) >= 5000)
	    {
		GET_QUESTPOINTS(ch) -= 5000;
	        obj = create_object(QUEST_ITEM8,GET_LEVEL(ch));
	    }
	    else
	    {
		sprintf(mybuf, "%s Sorry, but you don't have enough quest points for that.",GET_NAME(ch));
		do_tell(questman, mybuf, cmd_tell, 0);
                //send_to_char(buf, ch);
                //act(buf, FALSE, questman, 0, 0, TO_ROOM);
		return;
	    }
	}
	else if (isname(arg2, "#9 infinet infinite loop"))
	{
	    if (GET_QUESTPOINTS(ch) >= 4000)
	    {
		GET_QUESTPOINTS(ch) -= 4000;
	        obj = create_object(QUEST_ITEM9,GET_LEVEL(ch));
	    }
	    else
	    {
		sprintf(mybuf, "%s Sorry, but you don't have enough quest points for that.",GET_NAME(ch));
		do_tell(questman, mybuf, cmd_tell, 0);
                //send_to_char(buf, ch);
                //act(buf, FALSE, questman, 0, 0, TO_ROOM);
		return;
	    }
	}
	else if (isname(arg2, "#10 golden claw"))
	{
	    if (GET_QUESTPOINTS(ch) >= 3000)
	    {
		GET_QUESTPOINTS(ch) -= 3000;
	        obj = create_object(QUEST_ITEM10,GET_LEVEL(ch));
	    }
	    else
	    {
		sprintf(mybuf, "%s Sorry, but you don't have enough quest points for that.",GET_NAME(ch));
		do_tell(questman, mybuf, cmd_tell, 0);
                //send_to_char(buf, ch);
                //act(buf, FALSE, questman, 0, 0, TO_ROOM);
		return;
	    }
	}
	else if (isname(arg2, "#14 boots water walk walking"))
	{
	    if (GET_QUESTPOINTS(ch) >= 800)
	    {
		GET_QUESTPOINTS(ch) -= 800;
	        obj = create_object(QUEST_ITEM14,GET_LEVEL(ch));
	    }
	    else
	    {
		sprintf(mybuf, "%s Sorry, but you don't have enough quest points for that.",GET_NAME(ch));
		do_tell(questman, mybuf, cmd_tell, 0);
                //send_to_char(buf, ch);
                //act(buf, FALSE, questman, 0, 0, TO_ROOM);
		return;
	    }
	}
	else if (isname(arg2, "#13 practices prac practice"))
	{
	    if (GET_QUESTPOINTS(ch) >= 1000)
	    {
		GET_QUESTPOINTS(ch) -= 1000;
	        GET_PRACTICES(ch) += 30;
    	        act( "$N gives 30 practices to $n.",FALSE,
                       ch, NULL, questman, TO_ROOM );
    	        act( "$N gives you 30 practices.", FALSE,
                        ch, NULL, questman, TO_CHAR );
	        return;
	    }
	    else
	    {
		sprintf(mybuf, "%s Sorry, but you don't have enough quest points for that.",GET_NAME(ch));
		do_tell(questman, mybuf, cmd_tell, 0);
                //send_to_char(buf, ch);
                //act(buf, FALSE, questman, 0, 0, TO_ROOM);
		return;
	    }
	}
	else if (isname(arg2, "#12 train training"))
	{
	    if (GET_QUESTPOINTS(ch) >= 1500)
	    {
		GET_QUESTPOINTS(ch) -= 1500;
	        GET_TRAINING(ch) += 1;
    	        act( "$N gives 1 training session to $n.",FALSE,
                       ch, NULL, questman, TO_ROOM );
    	        act( "$N gives you 1 training session.", FALSE,
                        ch, NULL, questman, TO_CHAR );
	        return;
	    }
	    else
	    {
		sprintf(mybuf, "%s Sorry, but you don't have enough quest points for that.",GET_NAME(ch));
		do_tell(questman, mybuf, cmd_tell, 0);
                //send_to_char(buf, ch);
                //act(buf, FALSE, questman, 0, 0, TO_ROOM);
		return;
	    }
	}
	else if (isname(arg2, "#11 gold gp"))
	{
	    if (GET_QUESTPOINTS(ch) >= 2000)
	    {
		GET_QUESTPOINTS(ch) -= 2000;
	        GET_GOLD(ch) += 2000000;
    	        act( "$N gives 2,000,000 gold pieces to $n.",FALSE,
                         ch, NULL, questman, TO_ROOM );
    	        act( "$N has 2,000,000 in gold transfered from $s Swiss account to your balance.", 
                     FALSE,  ch, NULL, questman, TO_CHAR );
	        return;
	    }
	    else
	    {
		sprintf(mybuf, "%s Sorry, but you don't have enough quest points for that.",GET_NAME(ch));
		do_tell(questman, mybuf, cmd_tell, 0);
                //send_to_char(buf, ch);
                //act(buf, FALSE, questman, 0, 0, TO_ROOM);
		return;
	    }
	}
	else if (isname(arg2, "#15 brick"))
	{
	    if (GET_QUESTPOINTS(ch) >= 500)
	    {
		GET_QUESTPOINTS(ch) -= 500;
	        obj = create_object(QUEST_ITEM15,GET_LEVEL(ch));
	    }
	    else
	    {
		sprintf(mybuf, "%s Sorry, but you don't have enough quest points for that.",GET_NAME(ch));
		do_tell(questman, mybuf, cmd_tell, 0);
                //send_to_char(buf, ch);
                //act(buf, FALSE, questman, 0, 0, TO_ROOM);
		return;
	    }
	}
	else
	{
	    sprintf(mybuf, "%s I don't have that item, sorry.",GET_NAME(ch));
	    do_tell(questman, mybuf, cmd_tell, 0);
	    //send_to_char(buf, ch);
	    //act(buf, FALSE, questman, 0, 0, TO_ROOM);
	    return;
	}
	if (obj != NULL)
	{
    	    act( "$N gives $p to $n.",FALSE, ch, obj, questman, TO_ROOM );
    	    act( "$N gives you $p.",FALSE,   ch, obj, questman, TO_CHAR );
	    obj_to_char(obj, ch);
	}
	return;
    }
    else if (!strcmp(arg1, "request"))
    {
        act( "$n asks $N for a quest.",FALSE, ch, NULL, questman, TO_ROOM); 
	act ("You ask $N for a quest.",FALSE, ch, NULL, questman, TO_CHAR);
	if (IS_QUESTOR(ch))
	{
	    sprintf(mybuf, "%s But you're already on a quest!", GET_NAME(ch));
	    do_tell(questman, mybuf, cmd_tell, 0);
	    //send_to_char(buf, ch);
	    //act(buf, FALSE, questman, 0, 0, TO_ROOM);
	    return;
	}
	if (GET_NEXTQUEST(ch) > 0)
	{
	    sprintf(mybuf, "%s You're very brave, but let someone else have a chance.",GET_NAME(ch));
	    do_tell(questman, mybuf, cmd_tell, 0);
	    //send_to_char(buf, ch);
	    //act(buf, FALSE, questman, 0, 0, TO_ROOM);
	    sprintf(mybuf, "%s Come back later.", GET_NAME(ch));
	    do_tell(questman, mybuf, cmd_tell, 0);
	    //send_to_char(buf, ch);
	    //act(buf, FALSE, questman, 0, 0, TO_ROOM);
	    return;
	}

	sprintf(buf, "Thank you, brave %s!\r\n",GET_NAME(ch));
	send_to_char(buf, ch);
	//act(buf, FALSE, questman, 0, 0, TO_ROOM);
	generate_quest(ch, questman);
        if (GET_QUESTMOB(ch) > 0 || GET_QUESTOBJ(ch) > 0)
	{
            GET_COUNTDOWN(ch) = number(10,30);
	    SET_BIT(PLR_FLAGS(ch), PLR_QUESTOR);
	    sprintf(buf, "You have %d minutes to complete this quest.\r\n",
		    GET_COUNTDOWN(ch));
	    send_to_char(buf, ch);
	    //act(buf, FALSE, questman, 0, 0, TO_ROOM);
	    sprintf(buf, "May the gods go with you!\r\n");
	    send_to_char(buf, ch);
	    //act(buf, FALSE, questman, 0, 0, TO_ROOM);
	}
	return;
    }
    else if (!strcmp(arg1, "complete"))
    {
      act( "$n informs $N $e has completed $s quest.",FALSE, ch, NULL, questman, TO_ROOM); 
	  act ("You inform $N you have completed $s quest.",FALSE, ch, NULL, questman, TO_CHAR);
	if (GET_QUESTGIVER(ch) != questman)
	{
	    sprintf(mybuf, "%s I never sent you on a quest! Perhaps you're thinking of someone else.", GET_NAME(ch));
	    do_tell(questman, mybuf, cmd_tell, 0);
	    //send_to_char(buf, ch);
	    //act(buf, FALSE, questman, 0, 0, TO_ROOM);
	    return;
	}

	if (IS_QUESTOR(ch))
	{

	    if (GET_QUESTMOB(ch) < 0 && GET_COUNTDOWN(ch) > 0)
                {
		int reward, pointreward, pracreward;

		reward = number(1000,3000);
		pointreward = number(20,80);
		GET_QUESTMOB(ch) = abs(GET_QUESTMOB(ch));
		reward *= GET_QUESTMOB(ch);
		pointreward *= GET_QUESTMOB(ch);

		sprintf(mybuf, "%s Congratulations on completing your quest!",
			GET_NAME(ch));
		do_tell(questman, mybuf, cmd_tell, 0);
		//send_to_char(buf, ch);
                //act(buf, FALSE, questman, 0, 0, TO_ROOM);
		sprintf(mybuf,"%s As a reward, I am giving you %d quest points, and %d gold.",GET_NAME(ch),pointreward,reward);
		do_tell(questman, mybuf, cmd_tell, 0);
                //send_to_char(buf, ch);
                //act(buf, FALSE, questman, 0, 0, TO_ROOM);
		if (qchance(15))
		{
		    pracreward = number(1,6);
		    sprintf(buf, "You gain %d practices!\r\n",pracreward);
		    send_to_char(buf, ch);
		    GET_PRACTICES(ch) += pracreward;
		}

	        REMOVE_BIT(PLR_FLAGS(ch), PLR_QUESTOR);
                GET_QUESTGIVER(ch) = NULL;
                GET_COUNTDOWN(ch) = 0;
                GET_QUESTMOB(ch) = 0;
                GET_QUESTOBJ(ch) = 0;
                GET_NEXTQUEST(ch) = 15;
		GET_GOLD(ch) += reward;
		GET_QUESTPOINTS(ch) += pointreward;

	        return;
	    }
	    else if (GET_QUESTOBJ(ch) > 0 && GET_COUNTDOWN(ch) > 0)
	    {
		bool obj_found = FALSE;

    		for (obj = ch->carrying; obj != NULL; obj= obj_next)
    		{
        	    obj_next = obj->next_content;
        
		    if (obj != NULL && GET_OBJ_VNUM(obj) == GET_QUESTOBJ(ch))
		    {
			obj_found = TRUE;
            	        break;
		    }
        	}
		if (obj_found == TRUE)
		{
		    int reward, pointreward, pracreward;

		    reward = number(1500,25000);
		    pointreward = number(10,40);

		    act("You hand $p to $N.",FALSE,
                             ch, obj, questman, TO_CHAR);
		    act("$n hands $p to $N.",FALSE,
                         ch, obj, questman, TO_ROOM);

		    obj_from_char (obj);
		    extract_obj(obj);
	    	    sprintf(mybuf, "%s Congratulations on completing your quest!", GET_NAME(ch));
		    do_tell(questman, mybuf, cmd_tell, 0);
		    //send_to_char(buf, ch);
		    //act(buf, FALSE, questman, 0, 0, TO_ROOM);
		    sprintf(mybuf,"%s As a reward, I am giving you %d quest points, and %d gold.",GET_NAME(ch),pointreward,reward);
		    do_tell(questman, mybuf, cmd_tell, 0);
		    //send_to_char(buf, ch);
		    //act(buf, FALSE, questman, 0, 0, TO_ROOM);
		    if (qchance(15))
		    {
		        pracreward = number(1,6);
		        sprintf(buf, "You gain %d practices!\r\n",pracreward);
		        send_to_char(buf, ch);
		        GET_PRACTICES(ch) += pracreward;
		    }

	            REMOVE_BIT(PLR_FLAGS(ch), PLR_QUESTOR);
	            GET_QUESTGIVER(ch) = NULL;
	            GET_COUNTDOWN(ch) = 0;
	            GET_QUESTMOB(ch) = 0;
		    GET_QUESTOBJ(ch) = 0;
	            GET_NEXTQUEST(ch) = 30;
		    GET_GOLD(ch) += reward;
		    GET_QUESTPOINTS(ch) += pointreward;
		    return;
		}
		else
		{
		  sprintf(mybuf, "%s You haven't completed the quest yet, but there is still time!", GET_NAME(ch));
		  do_tell(questman, mybuf, cmd_tell, 0);
		  //send_to_char(buf, ch);
		  //act(buf, FALSE, questman, 0, 0, TO_ROOM);
		  return;
		}
		return;
	    }
	    else if ((GET_QUESTMOB(ch) > 0 || GET_QUESTOBJ(ch) > 0) && GET_COUNTDOWN(ch) > 0)
	    {
		sprintf(mybuf, "%s You haven't completed the quest yet, but there is still time!", GET_NAME(ch));
		do_tell(questman, mybuf, cmd_tell, 0);
                //send_to_char(buf, ch);
                //act(buf, FALSE, questman, 0, 0, TO_ROOM);
		return;
	    }
	}
	if (GET_NEXTQUEST(ch) > 0)
	  sprintf(mybuf,"%s But you didn't complete your quest in time!",
		  GET_NAME(ch));
	else 
	  sprintf(mybuf, "%s You have to REQUEST a quest first.",GET_NAME(ch));
	do_tell(questman, mybuf, cmd_tell, 0);
	//send_to_char(buf, ch);
	//act(buf, FALSE, questman, 0, 0, TO_ROOM);
	return;
    }

    send_to_char("QUEST commands: POINTS INFO TIME REQUEST COMPLETE LIST BUY.\r\n",ch);
    send_to_char("For more information, type 'HELP QUEST'.\r\n",ch);
    return;
}

void generate_quest(struct char_data *ch, struct char_data *questman)
{
    struct char_data *victim = NULL;
    struct char_data *vsearch = NULL, *tch = NULL;
    struct obj_data *questitem;
    char mybuf[MAX_STRING_LENGTH];
    int level_diff, r_num, mcounter, i, found;
    int cmd_tell = find_command("tell");

    /*  Randomly selects a mob from the world mob list. If you don't
	want a mob to be selected, make sure it is immune to summon.
	Or, you could add a new mob flag called ACT_NOQUEST. The mob
	is selected for both mob and obj quests, even tho in the obj
	quest the mob is not used. This is done to assure the level
	of difficulty for the area isn't too great for the player. */
/* added another mob flag - MOB_QUESTABLE - for mobs it is ok to quest
   against. */
    for (mcounter = 0; mcounter < 99; mcounter ++)
    { 
	r_num = number(1, top_of_mobt - 1);
         if ( (vsearch = read_mobile(r_num, REAL)) != NULL)
         { 
	   //level_diff = GET_LEVEL(vsearch) - GET_LEVEL(ch);
	   level_diff = estimate_difficulty(ch, vsearch); 
		/* Level differences to search for. Moongate has 350
		   levels, so you will want to tweak these greater or
		   less than statements for yourself. - Vassago */
            if(GET_LEVEL(ch) < LVL_IMMORT)	/* imm testing */	
	      if (level_diff > 15 || level_diff < 0 ||
		  !MOB_FLAGGED(vsearch, MOB_QUEST))
		{
		  char_to_room(vsearch, real_room(1204));
		  extract_char(vsearch);
		  vsearch = NULL;
		}
	    if(vsearch != NULL)
	      break;
         }
         
    }
    if(GET_LEVEL(ch) < LVL_IMMORT)  /* imm 100% */
    if(qchance(15) && vsearch != NULL) /* chance that a quest will be generated */
    {
     char_to_room(vsearch, real_room(1204));
     extract_char(vsearch);
     vsearch = NULL;
    }
    if(vsearch != NULL)
{
     found = 0;
     for(i = 0; i < top_of_world; i++)
     {
      for(tch = world[i].people;tch;tch = tch->next_in_room)
      {
       if(GET_MOB_VNUM(tch) == GET_MOB_VNUM(vsearch))
       {
        victim = tch;
        found = 1; 
        break;
       }
      }
      if(found)
       {
     char_to_room(vsearch, real_room(1204));
     extract_char(vsearch);
     vsearch = NULL;
        break;
       }
     }
}
    if (victim == NULL)
    {
      sprintf(mybuf, "%s Sorry, but I don't have any quests for you now. Try again later", GET_NAME(ch));
      do_tell(questman, mybuf, cmd_tell, 0);
      //send_to_char(buf, ch);
      //act(buf, FALSE, questman, 0, 0, TO_ROOM);
      //act(buf, FALSE, questman, 0, 0, TO_ROOM);
      GET_NEXTQUEST(ch) = 1;
      GET_QUESTMOB(ch) = 0;
      GET_QUESTOBJ(ch) = 0;
      GET_QUESTGIVER(ch) = NULL;
      GET_COUNTDOWN(ch) = 0;
      REMOVE_BIT(PLR_FLAGS(ch), PLR_QUESTOR);
      return;
    } 
 
    if (qchance(50))  /* prefer item quests myself */
    {
	int objvnum = 0;
	switch(number(0,4))
	{
	    case 0:
	    objvnum = QUEST_OBJQUEST1;
	    break;

	    case 1:
	    objvnum = QUEST_OBJQUEST2;
	    break;

	    case 2:
	    objvnum = QUEST_OBJQUEST3;
	    break;

	    case 3:
	    objvnum = QUEST_OBJQUEST4;
	    break;

	    case 4:
	    objvnum = QUEST_OBJQUEST5;
	    break;
	}
        questitem = create_object( objvnum, GET_LEVEL(ch) );
        if(questitem == NULL)
           {
           log("questitem does not EXIST!!");
           send_to_char("Error: questitem does not exist! please notify the imms\r\n",ch);
           return;
           }
	obj_to_room(questitem, victim->in_room);

        GET_QUESTOBJ(ch) = GET_OBJ_VNUM(questitem);
        sprintf(buf, "A rare and valuable %s has been stolen from the museum!\r\n",
             questitem->short_description);
	send_to_char(buf, ch);
	//act(buf, FALSE, questman, 0, 0, TO_ROOM);
	/* I changed my area names so that they have just the name of the area
	   and none of the level stuff. You may want to comment these next two
	   lines. - Vassago */
	sprintf(buf, "Look in the general area of %s for %s!\r\n",
          zone_table[world[victim->in_room].zone].name ,
              world[victim->in_room].name);
	send_to_char(buf, ch);
	//act(buf, FALSE, questman, 0, 0, TO_ROOM);
	return;
    }

    /* Quest to kill a mob */

    else 
    {
    switch(number(0,1))
    {
	case 0:
        sprintf(buf, "An enemy of mine, %s, is making vile threats against the crown.\r\n",
                 GET_NAME(victim));
	send_to_char(buf, ch);
	//act(buf, FALSE, questman, 0, 0, TO_ROOM);
        sprintf(buf, "This threat must be eliminated!\r\n");
	send_to_char(buf, ch);
	//act(buf, FALSE, questman, 0, 0, TO_ROOM);
	break;

	case 1:
	sprintf(buf, "Deltania's most heinous criminal, %s, has escaped from the dungeon!\r\n",
               GET_NAME(victim));
	send_to_char(buf, ch);
	//act(buf, FALSE, questman, 0, 0, TO_ROOM);
	sprintf(buf, "Since the escape, %s has murdered %d civilians!\r\n",
		GET_NAME(victim), number(2,20));
	send_to_char(buf, ch);
	//act(buf, FALSE, questman, 0, 0, TO_ROOM);
	break;
    }
    if (world[victim->in_room].name != NULL)
    {
        sprintf(buf, "Seek %s out somewhere in the vicinity of %s!\r\n",
		GET_NAME(victim),world[victim->in_room].name);
	send_to_char(buf, ch);
	//act(buf, FALSE, questman, 0, 0, TO_ROOM);

	/* I changed my area names so that they have just the name of the area
	   and none of the level stuff. You may want to comment these next two
	   lines. - Vassago */

	sprintf(buf, "That location is in the general area of %s.\r\n",
		zone_table[world[victim->in_room].zone].name);
	send_to_char(buf, ch);
	//act(buf, FALSE, questman, 0, 0, TO_ROOM);
    }
    GET_QUESTMOB(ch) = GET_MOB_VNUM(victim);
    }
    return;
}

/* Called from update_handler() by pulse_area */

void quest_update(void)
{
    struct char_data *ch, *ch_next;

    for ( ch = character_list; ch; ch = ch_next )
    {
        ch_next = ch->next;

	if (IS_NPC(ch)) continue;

	if (GET_NEXTQUEST(ch) > 0) 
	{
	    GET_NEXTQUEST(ch)--;

	    if (GET_NEXTQUEST(ch) == 0)
	    {
	        send_to_char("You may now quest again.\r\n",ch);
	        return;
	    }
	}
        else if (IS_QUESTOR(ch))
        {
	    if (--GET_COUNTDOWN(ch) <= 0)
	    {
	        GET_NEXTQUEST(ch) = 30;
	        sprintf(buf, "You have run out of time for your quest!\r\nYou may quest again in %d minutes.\r\n",GET_NEXTQUEST(ch));
	        send_to_char(buf, ch);
	        REMOVE_BIT(PLR_FLAGS(ch), PLR_QUESTOR);
                GET_QUESTGIVER(ch) = NULL;
                GET_COUNTDOWN(ch) = 0;
                GET_QUESTMOB(ch) = 0;
	    }
	    if (GET_COUNTDOWN(ch) > 0 && GET_COUNTDOWN(ch) < 6)
	    {
	        send_to_char("Better hurry, you're almost out of time for your quest!\r\n",ch);
	        return;
	    }
        }
    }
    return;
}
