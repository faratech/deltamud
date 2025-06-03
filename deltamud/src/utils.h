/* ************************************************************************
*   File: utils.h                                       Part of CircleMUD *
*  Usage: header file: utility macros and prototypes of utility funcs     *
*                                                                         *
*  All rights reserved.  See license.doc for complete information.        *
*                                                                         *
*  Copyright (C) 1993, 94 by the Trustees of the Johns Hopkins University *
*  CircleMUD is based on DikuMUD, Copyright (C) 1990, 1991.               *
************************************************************************ */


/* external declarations and prototypes **********************************/
int ismap(room_rnum room);
extern int top_of_objt;
extern struct weather_data weather_info;
#define log(x) basic_mud_log(x)

/* public functions in utils.c */
int GET_RACE_MIN(int race, int stat);
int GET_RACE_MAX(int race, int stat);
int chance (struct char_data *ch, struct char_data *vict, int type);
float dam_multi (struct char_data *ch, struct char_data *vict, int type);
double power ( double num, int power );
double mlog (double base, double num);
double	sqrt (double num);
sh_int	a2i(char *vnum);
char	*str_dup(const char *source);
int	str_cmp(const char *arg1, const char *arg2);
int	strn_cmp(char *arg1, char *arg2, int n);
void	basic_mud_log(char *str);
int	touch(char *path);
void	mudlog(char *str, char type, int level, byte file);
void	log_death_trap(struct char_data *ch);
void	search_replace(char *string, const char *find, const char *replace);
int	number(int from, int to);
int	dice(int number, int size);
void	sprintbit(long vektor, const char *names[], char *result);
void	sprinttype(int type, const char *names[], char *result);
int	get_line(FILE *fl, char *buf);
int	get_filename(char *orig_name, char *filename, int mode);
struct time_info_data age(struct char_data *ch);
int	num_pc_in_room(struct room_data *room);
int	replace_str(char **string, char *pattern, char *replacement, int rep_all, int max_size);
void	format_text(char **ptr_string, int mode, struct descriptor_data *d, int maxlen);
char	*stripcr(char *dest, const char *src);
int	item_count(struct char_data *ch);

/* random functions in random.c */
void circle_srandom(unsigned long initial_seed);
unsigned long circle_random(void);

/* undefine MAX and MIN so that our functions are used instead */
#ifdef MAX
#undef MAX
#endif

#ifdef MIN
#undef MIN
#endif

int MAX(int a, int b);
int MIN(int a, int b);

/* in magic.c */
bool	circle_follow(struct char_data *ch, struct char_data * victim);

/* in act.informative.c */
void	look_at_room(struct char_data *ch, int mode);

/* in act.movmement.c */
int	do_simple_move(struct char_data *ch, int dir, int following);
int	perform_move(struct char_data *ch, int dir, int following);

/* in limits.c */
int	mana_limit(struct char_data *ch);
int	hit_limit(struct char_data *ch);
int	move_limit(struct char_data *ch);
int	mana_gain(struct char_data *ch);
int	hit_gain(struct char_data *ch);
int	move_gain(struct char_data *ch);
void	advance_level(struct char_data *ch);
void	set_title(struct char_data *ch, char *title);
void	gain_exp(struct char_data *ch, int gain);
void	gain_exp_regardless(struct char_data *ch, int gain);
void	gain_condition(struct char_data *ch, int condition, int value);
void	check_idling(struct char_data *ch);
void	point_update(void);
void	update_pos(struct char_data *victim);


/* various constants *****************************************************/


/* defines for mudlog() */
#define OFF	0
#define BRF	1
#define NRM	2
#define PFT     3
#define CMP	4

/* get_filename() */
#define CRASH_FILE	0
#define ETEXT_FILE	1
#define ALIAS_FILE      2
#define EDATA_FILE      3

/* breadth-first searching */
#define BFS_ERROR		-1
#define BFS_ALREADY_THERE	-2
#define BFS_NO_PATH		-3

/* mud-life time */
#define SECS_PER_MUD_HOUR	75
#define SECS_PER_MUD_DAY	(24*SECS_PER_MUD_HOUR)
#define SECS_PER_MUD_MONTH	(35*SECS_PER_MUD_DAY)
#define SECS_PER_MUD_YEAR	(17*SECS_PER_MUD_MONTH)

/* real-life time (remember Real Life?) */
#define SECS_PER_REAL_MIN	60
#define SECS_PER_REAL_HOUR	(60*SECS_PER_REAL_MIN)
#define SECS_PER_REAL_DAY	(24*SECS_PER_REAL_HOUR)
#define SECS_PER_REAL_YEAR	(365*SECS_PER_REAL_DAY)


/* string utils **********************************************************/


#define YESNO(a) ((a) ? "YES" : "NO")
#define ONOFF(a) ((a) ? "ON" : "OFF")

#define LOWER(c)   (((c)>='A'  && (c) <= 'Z') ? ((c)+('a'-'A')) : (c))
#define UPPER(c)   (((c)>='a'  && (c) <= 'z') ? ((c)+('A'-'a')) : (c) )

#define ISNEWL(ch) ((ch) == '\n' || (ch) == '\r') 
#define IF_STR(st) ((st) ? (st) : "\0")
#define CAP(st)  (*(st) = UPPER(*(st)), st)

#define AN(string) (strchr("aeiouAEIOU", *string) ? "an" : "a")


/* memory utils **********************************************************/


#define CREATE(result, type, number)  do {\
	if (!((result) = (type *) calloc ((number), sizeof(type))))\
		{ perror("malloc failure"); fprintf(stderr,"malloc failure"); fflush(stderr); abort(); } } while(0)

#define RECREATE(result,type,number) do {\
  if (!((result) = (type *) realloc ((result), sizeof(type) * (number))))\
		{ perror("realloc failure"); fprintf(stderr,"malloc failure"); fflush(stderr);  abort(); } } while(0)


/*
 * the source previously used the same code in many places to remove an item
 * from a list: if it's the list head, change the head, else traverse the
 * list looking for the item before the one to be removed.  Now, we have a
 * macro to do this.  To use, just make sure that there is a variable 'temp'
 * declared as the same type as the list to be manipulated.  BTW, this is
 * a great application for C++ templates but, alas, this is not C++.  Maybe
 * CircleMUD 4.0 will be...
 */
#define REMOVE_FROM_LIST(item, head, next)	\
   if ((item) == (head))		\
      head = (item)->next;		\
   else {				\
      temp = head;			\
      while (temp && (temp->next != (item))) \
	 temp = temp->next;		\
      if (temp)				\
         temp->next = (item)->next;	\
   }					\


/* basic bitvector utils *************************************************/


#define IS_SET(flag,bit)  ((flag) & (bit))
#define SET_BIT(var,bit)  ((var) |= (bit))
#define REMOVE_BIT(var,bit)  ((var) &= ~(bit))
#define TOGGLE_BIT(var,bit) ((var) = (var) ^ (bit))

#define MOB_FLAGS(ch) ((ch)->char_specials.saved.act)
#define PLR_FLAGS(ch) ((ch)->char_specials.saved.act)
#define PRF_FLAGS(ch) ((ch)->player_specials->saved.pref)
#define PRF2_FLAGS(ch) ((ch)->player_specials->saved.pref2)
#define AFF_FLAGS(ch) ((ch)->char_specials.saved.affected_by)
#define BUP_AFF_FLAGS(ch) ((ch)->char_specials.bup_affected_by)
#define BUP_WIMP_LEV(ch) ((ch)->char_specials.bup_wimp_level)
#define BUP_RECALL_LEV(ch) ((ch)->char_specials.bup_recall_level)
#define BUP_RETREAT_LEV(ch) ((ch)->char_specials.bup_retreat_level)
#define ROOM_FLAGS(loc) (world[(loc)].room_flags)

#define IS_NPC(ch)  (IS_SET(MOB_FLAGS(ch), MOB_ISNPC))
#define IS_MOB(ch)  (IS_NPC(ch) && ((ch)->nr >-1))

#define MOB_FLAGGED(ch, flag) (IS_NPC(ch) && IS_SET(MOB_FLAGS(ch), (flag)))
#define PLR_FLAGGED(ch, flag) (!IS_NPC(ch) && IS_SET(PLR_FLAGS(ch), (flag)))
#define AFF_FLAGGED(ch, flag) (IS_SET(AFF_FLAGS(ch), (flag)))
#define PRF_FLAGGED(ch, flag) (IS_SET(PRF_FLAGS(ch), (flag)))
#define PRF2_FLAGGED(ch, flag) (IS_SET(PRF2_FLAGS(ch), (flag)))
#define ROOM_FLAGGED(loc, flag) (IS_SET(ROOM_FLAGS(loc), (flag)))
#define SPEC_EXIT_FLAGGED(exit, flag) (IS_SET((exit)->exit_info, (flag)))
#define GCMD_FLAGGED(ch, flag) (IS_SET(GCMD_FLAGS(ch), (flag)))
#define GCMD2_FLAGGED(ch, flag) (IS_SET(GCMD2_FLAGS(ch), (flag)))
#define GCMD3_FLAGGED(ch, flag) (IS_SET(GCMD3_FLAGS(ch), (flag)))
#define GCMD4_FLAGGED(ch, flag) (IS_SET(GCMD3_FLAGS(ch), (flag)))
#define EXIT_FLAGGED(exit, flag) (IS_SET((exit)->exit_info, (flag)))
#define GET_ROOM_VNUM(rnum)	((rnum) >= 0 && (rnum) <= top_of_world ? world[(rnum)].number : NOWHERE)
#define GET_ROOM_SPEC(room)	((room) >= 0 ? world[(room)].func : NULL)

/* IS_AFFECTED for backwards compatibility */
#define IS_AFFECTED(ch, skill) (AFF_FLAGGED((ch), (skill)))

#define PLR_TOG_CHK(ch,flag) ((TOGGLE_BIT(PLR_FLAGS(ch), (flag))) & (flag))
#define PRF_TOG_CHK(ch,flag) ((TOGGLE_BIT(PRF_FLAGS(ch), (flag))) & (flag))
#define PRF2_TOG_CHK(ch,flag) ((TOGGLE_BIT(PRF2_FLAGS(ch), (flag))) & (flag))

/* room utils ************************************************************/


#define SECT(room)	(world[(room)].sector_type)

#define IS_DARK(room)  ( !world[room].light && \
                         (ROOM_FLAGGED(room, ROOM_DARK) || \
                          ( ( SECT(room) != SECT_INSIDE && \
                              SECT(room) != SECT_CITY ) && \
                            (weather_info.sunlight == SUN_SET || \
                             weather_info.sunlight == SUN_DARK)) ) )

#define IS_LIGHT(room)  (!IS_DARK(room))

     /*#define IS_JURISDICTED(room)  ((SECT(room) == SECT_CITY) ||\
		    (ROOM_FLAGGED(room, ROOM_INDOORS)))
		    */

#define IS_JURISDICTED(room)  ((SECT(room) == SECT_CITY) ||\
		    (SECT(room) == SECT_INSIDE))

#define GET_ROOM_SPEC(room) ((room) >= 0 ? world[(room)].func : NULL)
#define RM_BLOOD(rm)   ((int)world[rm].blood)
#define RM_SNOW(rm)    ((int)world[rm].snow)

/* char utils ************************************************************/


#define IN_ROOM(ch)	((ch)->in_room)
#define GET_WAS_IN(ch)	((ch)->was_in_room)
#define GET_AGE(ch)     (age(ch).year)

#define GET_NAME(ch)    (IS_NPC(ch) ? \
			 (ch)->player.short_descr : (ch)->player.name)

#define GET_TITLE(ch)   ((ch)->player.title)
#define GET_LEVEL(ch)   ((ch)->player.level)
#define GET_TRUST_LEVEL(ch) \
                        ((ch)->player_specials->saved.trust)
#define GET_PASSWD(ch)	((ch)->player.passwd)

#define GET_CITIZEN(ch) ((ch)->player_specials->saved.citizen)

/*
 * I wonder if this definition of GET_REAL_LEVEL should be the definition
 * of GET_LEVEL?  JE
 */
#define GET_REAL_LEVEL(ch) \
   (ch->desc && ch->desc->original ? GET_LEVEL(ch->desc->original) : \
    GET_LEVEL(ch))

#define GET_CLASS(ch)   ((ch)->player.class)
#define GET_RACE(ch)    ((ch)->player.race)
#define GET_DEITY(ch)	((ch)->player.deity)
#define GET_HOME(ch)	((ch)->player.hometown)
#define GET_HEIGHT(ch)	((ch)->player.height)
#define GET_WEIGHT(ch)	((ch)->player.weight)
#define GET_SEX(ch)	((ch)->player.sex)

#define GET_STR(ch)     ((ch)->aff_abils.str)
#define GET_ADD(ch)     ((ch)->aff_abils.str_add)
#define GET_DEX(ch)     ((ch)->aff_abils.dex)
#define GET_INT(ch)     ((ch)->aff_abils.intel)
#define GET_WIS(ch)     ((ch)->aff_abils.wis)
#define GET_CON(ch)     ((ch)->aff_abils.con)
#define GET_CHA(ch)     ((ch)->aff_abils.cha)

#define GET_EXP(ch)	  ((ch)->points.exp)
#define GET_DEFENSE(ch)   ((ch)->points.defense)
#define GET_HIT(ch)	  ((ch)->points.hit)
#define GET_MAX_HIT(ch)	  ((ch)->points.max_hit)
#define GET_MOVE(ch)	  ((ch)->points.move)
#define GET_MAX_MOVE(ch)  ((ch)->points.max_move)
#define GET_MANA(ch)	  ((ch)->points.mana)
#define GET_MAX_MANA(ch)  ((ch)->points.max_mana)
#define GET_GOLD(ch)	  ((ch)->points.gold)
#define GET_BANK_GOLD(ch) ((ch)->points.bank_gold)
#define GET_MDEFENSE(ch)  ((ch)->points.mdefense)
#define GET_POWER(ch)     ((ch)->points.power)
#define GET_MPOWER(ch)    ((ch)->points.mpower)
#define GET_TECHNIQUE(ch) ((ch)->points.technique)

#define GET_POS(ch)	  ((ch)->char_specials.position)
#define GET_IDNUM(ch)	  ((ch)->char_specials.saved.idnum)
#define GET_ID(x)         ((x)->id)
#define IS_CARRYING_W(ch) ((ch)->char_specials.carry_weight)
#define IS_CARRYING_N(ch) ((ch)->char_specials.carry_items)
#define FIGHTING(ch)	  ((ch)->char_specials.fighting)
#define LASTFIGHTING(ch)  ((ch)->char_specials.last_fighting)
#define HUNTING(ch)	  ((ch)->char_specials.hunting)
#define RIDING(ch)	  ((ch)->char_specials.riding)		// (DAK)
#define RIDDEN_BY(ch)	  ((ch)->char_specials.ridden_by)	// (DAK)
#define OBSERVING(ch)	  ((ch)->char_specials.observing)	// Thargor
#define OBSERVE_BY(ch)	  ((ch)->char_specials.observe_by)	// Thargor
#define BUP_AFFECTED(ch)  ((ch)->char_specials.bup_affected)	// Thargor
#define LOCK_PASSWD(ch)	  ((ch)->char_specials.lockpwd)   	// Thargor
#define GET_ALIGNMENT(ch) ((ch)->char_specials.saved.alignment)
#define GET_ARENASTAT(ch) ((ch)->player_specials->saved.arena)
#define GET_ARENAWINS(ch) ((ch)->player_specials->saved.wins)
#define GET_ARENALOSSES(ch) ((ch)->player_specials->saved.losses)
#define IS_ARENACOMBATANT(ch) (GET_ARENASTAT(ch) >= ARENA_COMBATANT1 && GET_ARENASTAT(ch) <= ARENA_COMBATANTZ)
#define GET_ARENAFLEETIMER(ch) ((ch)->char_specials.flee_timer)

#define GET_COND(ch, i)		((ch)->player_specials->saved.conditions[(i)])
#define GET_LOADROOM(ch)	((ch)->player_specials->saved.load_room)
#define GET_PRACTICES(ch)	((ch)->player_specials->saved.spells_to_learn)
#define GET_INVIS_LEV(ch)	((ch)->player_specials->saved.invis_level)
#define GET_WIMP_LEV(ch)	((ch)->player_specials->saved.wimp_level)
#define GET_BAIL_AMT(ch)	((ch)->player_specials->saved.bail_amt)
#define GET_RECALL_LEV(ch)      ((ch)->player_specials->saved.recall_level)
#define GET_RETREAT_LEV(ch)	((ch)->player_specials->saved.retreat_level)
#define GET_FREEZE_LEV(ch)	((ch)->player_specials->saved.freeze_level)
#define GET_BAD_PWS(ch)		((ch)->player_specials->saved.bad_pws)
#define GET_TALK(ch, i)		((ch)->player_specials->saved.talks[i])
#define POOFIN(ch)		((ch)->player_specials->poofin)
#define POOFOUT(ch)		((ch)->player_specials->poofout)
#define EMAIL(ch)               ((ch)->player_specials->email)
#define GET_LAST_OLC_TARG(ch)	((ch)->player_specials->last_olc_targ)
#define GET_LAST_OLC_MODE(ch)	((ch)->player_specials->last_olc_mode)
#define GET_ALIASES(ch)		((ch)->player_specials->aliases)
#define GET_LAST_TELL(ch)	((ch)->player_specials->last_tell)
#define GET_QUESTMOB(ch)	((ch)->player_specials->saved.questmob)
#define GET_QUESTOBJ(ch)	((ch)->player_specials->saved.questobj)
#define GET_QUESTGIVER(ch)	((ch)->player_specials->saved.questgiver)
#define GET_NEXTQUEST(ch)	((ch)->player_specials->saved.nextquest)
#define GET_QUESTPOINTS(ch)	((ch)->player_specials->saved.questpoints)
#define GET_COUNTDOWN(ch)	((ch)->player_specials->saved.countdown)
#define GET_TRAINING(ch)        ((ch)->player_specials->saved.training)
#define GET_CLAN(ch)            ((ch)->player_specials->saved.clan)
#define GET_CLAN_RANK(ch)       ((ch)->player_specials->saved.clan_rank)
#define GET_ATTACKTYPE(ch)      (GET_EQ(ch, WEAR_WIELD) ? (GET_EQ(ch, WEAR_WIELD)->obj_flags.value[3] < NUM_ATTACK_TYPES && GET_EQ(ch, WEAR_WIELD)->obj_flags.value[3] > -1 && GET_OBJ_TYPE(GET_EQ(ch, WEAR_WIELD)) == ITEM_WEAPON ? GET_EQ(ch, WEAR_WIELD)->obj_flags.value[3] + TYPE_HIT : TYPE_HIT) : (IS_NPC(ch) ? (ch->mob_specials.attack_type < NUM_ATTACK_TYPES && ch->mob_specials.attack_type > -1 ? ch->mob_specials.attack_type + TYPE_HIT : TYPE_HIT) : TYPE_HIT))

#define GET_SKILL(ch, i)	((ch)->player_specials->saved.skills[i])
#define SET_SKILL(ch, i, pct)	{ (ch)->player_specials->saved.skills[i] = pct; }

#define GET_EQ(ch, i)		((ch)->equipment[i])

#define GET_MOB_SPEC(ch) (IS_MOB(ch) ? (mob_index[(ch->nr)].func) : NULL)
#define GET_MOB_RNUM(mob)	((mob)->nr)
#define GET_MOB_VNUM(mob)	(IS_MOB(mob) ? \
				 mob_index[GET_MOB_RNUM(mob)].vnum : -1)

#define GET_MOB_WAIT(ch)	((ch)->mob_specials.wait_state)
#define GET_DEFAULT_POS(ch)	((ch)->mob_specials.default_pos)
#define MEMORY(ch)		((ch)->mob_specials.memory)

#define STRENGTH_APPLY_INDEX(ch) \
        ( ((GET_ADD(ch)==0) || (GET_STR(ch) != 18)) ? GET_STR(ch) :\
          (GET_ADD(ch) <= 50) ? 26 :( \
          (GET_ADD(ch) <= 75) ? 27 :( \
          (GET_ADD(ch) <= 90) ? 28 :( \
          (GET_ADD(ch) <= 99) ? 29 :  30 ) ) )                   \
        )

#define CAN_CARRY_W(ch) (str_app[STRENGTH_APPLY_INDEX(ch)].carry_w)
#define CAN_CARRY_N(ch) (5 + (GET_DEX(ch) >> 1) + (GET_LEVEL(ch) >> 1))
#define AWAKE(ch) (GET_POS(ch) > POS_SLEEPING)
#define CAN_SEE_IN_DARK(ch) \
   (AFF_FLAGGED(ch, AFF_INFRAVISION) || PRF_FLAGGED(ch, PRF_HOLYLIGHT))

#define IS_GOOD(ch)    (GET_ALIGNMENT(ch) >= 350)
#define IS_EVIL(ch)    (GET_ALIGNMENT(ch) <= -350)
#define IS_NEUTRAL(ch) (!IS_GOOD(ch) && !IS_EVIL(ch))


/* descriptor-based utils ************************************************/


#define WAIT_STATE(ch, cycle) { \
	if ((ch)->desc) (ch)->desc->wait = (cycle); \
	else if (IS_NPC(ch)) GET_MOB_WAIT(ch) = (cycle); }

#define CHECK_WAIT(ch)	(((ch)->desc) ? ((ch)->desc->wait > 1) : 0)
#define STATE(d)	((d)->connected)


/* object utils **********************************************************/


#define GET_OBJ_TYPE(obj)	((obj)->obj_flags.type_flag)
#define GET_OBJ_COST(obj)	((obj)->obj_flags.cost)
#define GET_OBJ_RENT(obj)	((obj)->obj_flags.cost_per_day)
#define GET_OBJ_EXTRA(obj)	((obj)->obj_flags.extra_flags)
#define GET_OBJ_WEAR(obj)	((obj)->obj_flags.wear_flags)
#define GET_OBJ_CSLOTS(obj)     ((obj)->obj_flags.curr_slots)
#define GET_OBJ_TSLOTS(obj)     ((obj)->obj_flags.total_slots)
#define GET_OBJ_VAL(obj, val)	((obj)->obj_flags.value[(val)])
#define GET_OBJ_WEIGHT(obj)	((obj)->obj_flags.weight)
#define GET_OBJ_TIMER(obj)	((obj)->obj_flags.timer)
#define GET_OBJ_RNUM(obj)	((obj)->item_number)
#define GET_OBJ_VNUM(obj)	(GET_OBJ_RNUM(obj) >= 0 && GET_OBJ_RNUM(obj) <= top_of_objt ? \
				 obj_index[GET_OBJ_RNUM(obj)].vnum : -1)
#define IS_OBJ_STAT(obj,stat)	(IS_SET((obj)->obj_flags.extra_flags,stat))
#define IS_CORPSE(obj)		(GET_OBJ_TYPE(obj) == ITEM_CONTAINER && \
					GET_OBJ_VAL((obj), 3) == 1)

#define GET_OBJ_SPEC(obj) ((obj)->item_number >= 0 ? \
	(obj_index[(obj)->item_number].func) : NULL)

#define CAN_WEAR(obj, part) (IS_SET((obj)->obj_flags.wear_flags, (part)))


/* compound utilities and other macros **********************************/


#define HSHR(ch) (GET_SEX(ch) ? (GET_SEX(ch)==SEX_MALE ? "his":"her") :"its")
#define HSSH(ch) (GET_SEX(ch) ? (GET_SEX(ch)==SEX_MALE ? "he" :"she") : "it")
#define HMHR(ch) (GET_SEX(ch) ? (GET_SEX(ch)==SEX_MALE ? "him":"her") : "it")
#define CHSHR(ch) (GET_SEX(ch) ? (GET_SEX(ch)==SEX_MALE ? "His":"Her") :"Its")
#define CHSSH(ch) (GET_SEX(ch) ? (GET_SEX(ch)==SEX_MALE ? "He" :"She") : "It")
#define CHMHR(ch) (GET_SEX(ch) ? (GET_SEX(ch)==SEX_MALE ? "Him":"Her") : "It")

#define ANA(obj) (strchr("aeiouyAEIOUY", *(obj)->name) ? "An" : "A")
#define SANA(obj) (strchr("aeiouyAEIOUY", *(obj)->name) ? "an" : "a")


/* Various macros building up to CAN_SEE */

#define LIGHT_OK(sub)	(!IS_AFFECTED(sub, AFF_BLIND) && \
   (IS_LIGHT((sub)->in_room) || IS_AFFECTED((sub), AFF_INFRAVISION)))

#define INVIS_OK(sub, obj) \
 ((!IS_AFFECTED((obj),AFF_INVISIBLE) || IS_AFFECTED(sub,AFF_DETECT_INVIS)) && \
 (!IS_AFFECTED((obj), AFF_HIDE) || IS_AFFECTED(sub, AFF_SENSE_LIFE)))

#define MORT_CAN_SEE(sub, obj) (LIGHT_OK(sub) && INVIS_OK(sub, obj) && (!PRF2_FLAGGED(obj, PRF2_INTANGIBLE) || PRF2_FLAGGED(sub, PRF2_INTANGIBLE) || PRF2_FLAGGED(obj, PRF2_MBUILDING)))

#define IMM_CAN_SEE(sub, obj) \
   (MORT_CAN_SEE(sub, obj) || PRF_FLAGGED(sub, PRF_HOLYLIGHT))

#define SELF(sub, obj)  ((sub) == (obj))

/* Can subject see character "obj"? */
#define CAN_SEE(sub, obj) (SELF(sub, obj) || \
   ((GET_REAL_LEVEL(sub) >= GET_INVIS_LEV(obj)) && IMM_CAN_SEE(sub, obj)))

/* End of CAN_SEE */


#define INVIS_OK_OBJ(sub, obj) \
  (!IS_OBJ_STAT((obj), ITEM_INVISIBLE) || IS_AFFECTED((sub), AFF_DETECT_INVIS))

#define MORT_CAN_SEE_OBJ(sub, obj) (LIGHT_OK(sub) && INVIS_OK_OBJ(sub, obj))

#define CAN_SEE_OBJ(sub, obj) \
   (MORT_CAN_SEE_OBJ(sub, obj) || PRF_FLAGGED((sub), PRF_HOLYLIGHT))

#define CAN_CARRY_OBJ(ch,obj)  \
   (((IS_CARRYING_W(ch) + GET_OBJ_WEIGHT(obj)) <= CAN_CARRY_W(ch)) &&   \
    ((IS_CARRYING_N(ch) + 1) <= CAN_CARRY_N(ch)))

#define CAN_GET_OBJ(ch, obj)   \
   (CAN_WEAR((obj), ITEM_WEAR_TAKE) && CAN_CARRY_OBJ((ch),(obj)) && \
    CAN_SEE_OBJ((ch),(obj)))


#define PERS(ch, vict)   (CAN_SEE(vict, ch) ? GET_NAME(ch) : (GET_LEVEL(ch) >= LVL_IMMORT ? "A Mystical Being" : "someone"))

#define OBJS(obj, vict) (CAN_SEE_OBJ((vict), (obj)) ? \
	(obj)->short_description  : "something")

#define OBJN(obj, vict) (CAN_SEE_OBJ((vict), (obj)) ? \
	fname((obj)->name) : "something")


#define EXIT(ch, door)	(world[(ch)->in_room].dir_option[door])
#define SPEC_EXIT(rnum)	(world[rnum].special_exit)

#define CAN_GO(ch, door) (EXIT(ch,door) && \
			 (EXIT(ch,door)->to_room != NOWHERE) && \
			 !IS_SET(EXIT(ch, door)->exit_info, EX_CLOSED) && \
                         (world[EXIT(ch,door)->to_room].mapmv!=-1 || !ismap(EXIT(ch,door)->to_room)))


#define CLASS_ABBR(ch) (IS_NPC(ch) ? "--" : class_abbrevs[(int)GET_CLASS(ch)])
#define RACE_ABBR(ch) (IS_NPC(ch) ? "--" : race_abbrevs[(int)GET_RACE(ch)])
#define DEITY_ABBR(ch)  (IS_NPC(ch) ? "--" : deity_abbrevs[(int)GET_DEITY(ch)])

#define IS_MAGIC_USER(ch)	(!IS_NPC(ch) && \
				(GET_CLASS(ch) == CLASS_MAGIC_USER))
#define IS_CLERIC(ch)		(!IS_NPC(ch) && \
				(GET_CLASS(ch) == CLASS_CLERIC))
#define IS_THIEF(ch)		(!IS_NPC(ch) && \
				(GET_CLASS(ch) == CLASS_THIEF))
#define IS_WARRIOR(ch)		(!IS_NPC(ch) && \
				(GET_CLASS(ch) == CLASS_WARRIOR))
#define IS_ARTISAN(ch)		(!IS_NPC(ch) && \
				GET_CLASS(ch) == CLASS_ARTISAN))

#define OUTSIDE(ch) (!ROOM_FLAGGED((ch)->in_room, ROOM_INDOORS))


/* OS compatibility ******************************************************/


/* there could be some strange OS which doesn't have NULL... */
#ifndef NULL
#define NULL (void *)0
#endif

#if !defined(FALSE)
#define FALSE 0
#endif

#if !defined(TRUE)
#define TRUE  (!FALSE)
#endif

/* defines for fseek */
#ifndef SEEK_SET
#define SEEK_SET	0
#define SEEK_CUR	1
#define SEEK_END	2
#endif

/*
 * NOCRYPT can be defined by an implementor manually in sysdep.h.
 * CIRCLE_CRYPT is a variable that the 'configure' script
 * automatically sets when it determines whether or not the system is
 * capable of encrypting.
 */
/* Password encryption system - upgraded to SHA-256 */
#if defined(NOCRYPT) || !defined(CIRCLE_CRYPT)
#define CRYPT(a,b) (a)
#else
#define CRYPT(a,b) ((char *) crypt((a),(b)))
#endif

/* New secure password functions */
char *create_secure_password_hash(const char *password, const char *username);
int verify_password(const char *password, const char *stored_hash, const char *username);
char *generate_salt(void);
int password_needs_upgrade(const char *stored_hash);

#define SENDOK(ch) ((IS_NPC(ch) || (ch)->desc) && (AWAKE(ch) || sleep) && \
                    !PLR_FLAGGED((ch), PLR_WRITING))

#define GCMD_FLAGS(ch)          ((ch)->player_specials->saved.godcmds1)
#define GCMD2_FLAGS(ch)         ((ch)->player_specials->saved.godcmds2)
#define GCMD3_FLAGS(ch)         ((ch)->player_specials->saved.godcmds3)
#define GCMD4_FLAGS(ch)         ((ch)->player_specials->saved.godcmds4)

#define IS_GOD(ch)              (!IS_NPC(ch) && (GCMD_FLAGS(ch) || GCMD2_FLAGS(ch) || \
                                  GCMD3_FLAGS(ch) || GCMD4_FLAGS(ch)))

#define READ_CITIZEN(ch)	(GET_SEX(ch) == SEX_MALE ?   \
        citizen_titles[(int)GET_CITIZEN(ch)].citizen_m : \
        GET_SEX(ch) == SEX_FEMALE ?  \
        citizen_titles[(int)GET_CITIZEN(ch)].citizen_f: \
        citizen_titles[(int)GET_CITIZEN(ch)].citizen_n)
