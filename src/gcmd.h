/*************************************************************************
*  File: gcmd.h                                      Part of DeltaMUD    *
*  Usage: Defines for god-commands                         22/7/98       *
*  Written by: Raf                                                       *
*  Copyright (c) 1997-1998, Abysmal MUD / Delta MUD                      *
*************************************************************************/

#ifndef _GCMD_H
#define _GCMD_H

/* Immortal command flags:  Used by player.godcmds1 */
#define GCMD_GEN            (1 << 0)  /*  General god commands */
#define GCMD_ADVANCE        (1 << 1)
#define GCMD_AT             (1 << 2)
#define GCMD_BAN            (1 << 3)
#define GCMD_DC             (1 << 4)
#define GCMD_ECHO           (1 << 5)
#define GCMD_FORCE          (1 << 6)
#define GCMD_FREEZE         (1 << 7)
#define GCMD_HCONTROL       (1 << 8)
#define GCMD_LOAD           (1 << 9)
#define GCMD_MUTE           (1 << 10)
#define GCMD_SYSLOG         (1 << 11) 
#define GCMD_PARDON         (1 << 12)
#define GCMD_PURGE          (1 << 13)
#define GCMD_RELOAD         (1 << 14)
#define GCMD_REROLL         (1 << 15)
#define GCMD_RESTORE        (1 << 16)
#define GCMD_SEND           (1 << 17)
#define GCMD_SET            (1 << 18)
#define GCMD_SHUTDOWN       (1 << 19)
#define GCMD_SKILLSET       (1 << 20)
#define GCMD_AUCTIONEER     (1 << 21)
#define GCMD_SLOWNS         (1 << 22)
#define GCMD_SNOOP          (1 << 23)
#define GCMD_SWITCH         (1 << 24)
#define GCMD_PLAGUE         (1 << 25)
#define GCMD_TRANS          (1 << 26)
#define GCMD_UNAFFECT       (1 << 27)
#define GCMD_WIZLOCK        (1 << 28)
#define GCMD_ISAY           (1 << 29)
#define GCMD_CMDSET         (1 << 31)  /* Don't change the pos. of this! */

/* Immortal command flags:  Used by player.godcmds2 */
#define GCMD2_OLC           (1 << 0)
#define GCMD2_INVIS         (1 << 1)
#define GCMD2_MCASTERS      (1 << 2)
#define GCMD2_MUDHEAL       (1 << 3)
#define GCMD2_REWIZ         (1 << 4)
#define GCMD2_GECHO         (1 << 5)
#define GCMD2_REPOWER       (1 << 6)
#define GCMD2_REWWW         (1 << 7)
#define GCMD2_NOTITLE       (1 << 8)
#define GCMD2_PAGE          (1 << 9)
#define GCMD2_QECHO         (1 << 10)
#define GCMD2_ZRESET        (1 << 11)
#define GCMD2_SETREBOOT     (1 << 12)
#define GCMD2_TMOBDIE       (1 << 13)
#define GCMD2_WRESTRICT     (1 << 14)
#define GCMD2_ATTACH        (1 << 15)
#define GCMD2_IMP           (1 << 26)  /* Implementor commands */
#define GCMD2_USERS         (1 << 27)
#define GCMD2_ALOAD         (1 << 28)
#define GCMD2_RESPEC        (1 << 29)
#define GCMD2_QUESTMOBS     (1 << 30)
#define GCMD2_REWARD        (1 << 31)

/* Immortal command flags:  Used by player.godcmds3 */
#define GCMD3_PEACE	    (1 << 0)
#define GCMD3_IMPOLC	    (1 << 1)
#define GCMD3_ADDSNOW       (1 << 2)
#define GCMD3_DELSNOW       (1 << 3)
#define GCMD3_COPYOVER      (1 << 4)
#define GCMD3_LWEATHER	    (1 << 5)
#define GCMD3_MAP	    (1 << 6)
#define GCMD3_PFILECLEAN    (1 << 7)
#define GCMD3_REBALANCE     (1 << 8)

/* Immortal command flags:  Used by player.godcmds4 */
#define GCMD4_NOTHING	    (1 << 0) /* this does nothing */

#endif

