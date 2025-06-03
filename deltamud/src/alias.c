/***************************************************************************
*  File: alias.c                                 an addition to CircleMUD  *
*  Usage: saving player's aliases                                          *
*                                                                          *
*  a drop-in replacement for alias.c                                       *
*  written by Edward Almasy (almasy@axis.com)                              *
*                                                                          *
*  (original alias.c by Jeremy Hess and Chad Thompson)                     *
*                                                                          *
*  Copyright (C) 1993, 94 by the Trustees of the Johns Hopkins University  *
*  CircleMUD is based on DikuMUD, Copyright (C) 1990, 1991.                *
***************************************************************************/

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "conf.h"
#include "sysdep.h"
#include "structs.h"
#include "utils.h"
#include "interpreter.h"

void get_arg(char *string, int argnum, char *arg);

void write_extra_data(struct char_data *ch)
{
    FILE         *ptFHndl=NULL;
    char          pcFileName[127];
    char buf2[MAX_STRING_LENGTH]="\0";

    get_filename(GET_NAME(ch), pcFileName, EDATA_FILE);
    unlink(pcFileName);
    ptFHndl = fopen(pcFileName,"w");
    if (!ptFHndl) return;
    if (EMAIL(ch))
      sprintf(buf2+strlen(buf2), "EMAIL %s\n", EMAIL(ch));
    if (POOFIN(ch))
      sprintf(buf2+strlen(buf2), "POOFIN %s\n", POOFIN(ch));
    if (POOFOUT(ch))
      sprintf(buf2+strlen(buf2), "POOFOUT %s\n", POOFOUT(ch));
    fputs(buf2, ptFHndl);
    fclose(ptFHndl);
}

void read_extra_data(struct char_data *ch)
{   
    FILE         *ptFHndl=NULL;
    char          pcFileName[127];
    char buf1[MAX_STRING_LENGTH]="\0";
    char buf2[MAX_STRING_LENGTH]="\0";
    
    get_filename(GET_NAME(ch), pcFileName, EDATA_FILE);
    ptFHndl = fopen(pcFileName, "r");
    
    if (!ptFHndl) return;
    
    if (EMAIL(ch))
      free(EMAIL(ch));
    if (POOFIN(ch))
      free(POOFIN(ch));
    if (POOFOUT(ch))
      free(POOFOUT(ch));

    while (!feof(ptFHndl))
    {
      fgets(buf1, 512, ptFHndl);
      get_arg(buf1, 1, buf2);
      for (;buf1[strlen(buf1)-1]=='\r' || buf1[strlen(buf1)-1]=='\n';) buf1[strlen(buf1)-1]='\0';
      if (!strcmp("EMAIL", buf2))
        EMAIL(ch)=strdup(buf1+strlen(buf2)+1);
      if (!strcmp("POOFIN", buf2))
        POOFIN(ch)=strdup(buf1+strlen(buf2)+1);
      if (!strcmp("POOFOUT", buf2))
        POOFOUT(ch)=strdup(buf1+strlen(buf2)+1);
    }
    fclose(ptFHndl);
}

void write_aliases(struct char_data *ch)
{
    FILE         *ptFHndl;
    char          pcFileName[127];
    struct alias *pstAliasRec;
    char buf2[MAX_STRING_LENGTH];

    write_extra_data(ch);
    /* get name of alias file */
    get_filename(GET_NAME(ch), pcFileName, ALIAS_FILE);
  
    /* remove old alias file */
    unlink(pcFileName);
  
    if (!GET_ALIASES(ch))
        return;
  
    /* open new alias file */
    ptFHndl = fopen(pcFileName,"w");
  
    /* while there are alias records left */
    pstAliasRec = GET_ALIASES(ch);
    while (pstAliasRec)
    {
        sprintf(buf2, "%s\n%s\n%d\n", pstAliasRec->alias, pstAliasRec->replacement, pstAliasRec->type);
        fputs(buf2, ptFHndl);
       
        /* move to next alias record */
        pstAliasRec = pstAliasRec->next;
    }
    
    /* close new alias file */
    fclose(ptFHndl);
}


void read_aliases(struct char_data *ch)
{   
    char          pcFileName[127];
    FILE         *ptFHndl;
    struct alias *pstAliasRec, *pstLast=NULL;
    extern void free_alias (struct alias *a);
    char buf[MAX_STRING_LENGTH];
    
    read_extra_data(ch);
    /* get alias file name */
    get_filename(GET_NAME(ch), pcFileName, ALIAS_FILE);
    
    ptFHndl = fopen(pcFileName, "r");
    
    if (ptFHndl == NULL)
        return;
    
    while ((pstAliasRec=GET_ALIASES(ch))!=NULL) {
      GET_ALIASES(ch)=(GET_ALIASES(ch))->next;
      free_alias(pstAliasRec);
    }
    
    CREATE(pstAliasRec, struct alias, 1);
    GET_ALIASES(ch) = pstAliasRec;

    /* while not end of alias file */
    while (!feof(ptFHndl))
    {
        fgets(buf, 512, ptFHndl);
        if (feof(ptFHndl)) break;
        while(buf[strlen(buf)-1]=='\n' || buf[strlen(buf)-1]=='\r')
          buf[strlen(buf)-1]='\0';
        pstAliasRec->alias=str_dup(buf);
        fgets(buf, 512, ptFHndl);
        if (feof(ptFHndl)) break;
        while(buf[strlen(buf)-1]=='\n' || buf[strlen(buf)-1]=='\r')
          buf[strlen(buf)-1]='\0';
        pstAliasRec->replacement=str_dup(buf);
        fgets(buf, 512, ptFHndl);
        while(buf[strlen(buf)-1]=='\n' || buf[strlen(buf)-1]=='\r')
          buf[strlen(buf)-1]='\0';
        pstAliasRec->type=atoi(buf);
        CREATE(pstAliasRec->next, struct alias, 1);
        pstLast=pstAliasRec;
        pstAliasRec = pstAliasRec->next;
    }
    if (pstAliasRec)
      free_alias(pstAliasRec);
    if (pstLast)
      pstLast->next=NULL;
    /* close alias file */
    fclose(ptFHndl);
} 