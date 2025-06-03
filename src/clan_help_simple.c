void clan_help(struct char_data *ch, char *arg)
{
   if(!*arg) {
      send_to_char("Clan help topics: score, roster, who, info, list, rname, privilege, "
                   "expel, resign, demote, promote, enlist, apply, rank, withdraw, "
                   "ctitle, talk, deposit\r\n", ch);
      return;
   }
   
   send_to_char("Help for clan commands is currently unavailable.\r\n", ch);
}