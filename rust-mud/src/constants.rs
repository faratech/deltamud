//! Constant lookup tables ported 1:1 from `src/constants.c` (CircleMUD 3.0 /
//! DeltaMUD).
//!
//! These are the named string / number arrays used for `sprintbit` /
//! `sprinttype` rendering and assorted game data. **Index order is the
//! contract** — `sprintbit` walks bit positions and `sprinttype` indexes by
//! ordinal, so every array preserves the exact C ordering and exact strings.
//! Where the C source terminates an array with a `"\n"` sentinel (the marker
//! `sprinttype`/`fread_string` use to detect "end of table"), that `"\n"` is
//! kept here as a literal `"\n"` element.
//!
//! Self-contained: no external dependencies.

/// `const char circlemud_version[]` — banner string.
pub static CIRCLEMUD_VERSION: &str =
    "DeltaMUD v3.0 - Beta Test\r\nBased on CircleMUD v3.0bpl12\r\n";

/// `dirs[]` — cardinal direction names (terminated with `"\n"`).
pub static DIRS: &[&str] = &["north", "east", "south", "west", "up", "down", "\n"];

/// `room_bits[]` — ROOM_x flags (terminated with `"\n"`).
pub static ROOM_BITS: &[&str] = &[
    "DARK",       // ROOM_DARK
    "DEATH",      // ROOM_DEATH
    "!MOB",       // ROOM_NOMOB
    "INDOORS",    // ROOM_INDOORS
    "PEACEFUL",   // ROOM_PEACEFUL
    "SOUNDPROOF", // ROOM_SOUNDPROOF
    "!TRACK",     // ROOM_NOTRACK
    "!MAGIC",     // ROOM_NOMAGIC
    "TUNNEL",     // ROOM_TUNNEL
    "PRIVATE",    // ROOM_PRIVATE
    "GODROOM",    // ROOM_GODROOM
    "HOUSE",      // ROOM_HOUSE
    "HCRSH",      // ROOM_HOUSE_CRASH
    "ATRIUM",     // ROOM_ATRIUM
    "OLC",        // ROOM_OLC
    "*",          // ROOM_BFS_MARK
    "IMPROOM",
    "BAD_REGEN",
    "WALL",
    "CLANROOM",
    "GOOD_REGEN",
    "\n",
];

/// `exit_bits[]` — EX_x exit flags (terminated with `"\n"`).
pub static EXIT_BITS: &[&str] = &[
    "DOOR",      // EX_ISDOOR
    "CLOSED",    // EX_CLOSED
    "LOCKED",    // EX_LOCKED
    "PICKPROOF", // EX_PICKPROOF
    "\n",
];

/// `sector_types[]` — SECT_x sector types (terminated with `"\n"`).
pub static SECTOR_TYPES: &[&str] = &[
    "Inside",          // SECT_INSIDE
    "City",            // SECT_CITY
    "Field",           // SECT_FIELD
    "Forest",          // SECT_FOREST
    "Hills",           // SECT_HILLS
    "Mountains",       // SECT_MOUNTAIN
    "Water (Swim)",    // SECT_WATER_SWIM
    "Water (No Swim)", // SECT_WATER_NOSWIM
    "Underwater",      // SECT_UNDERWATER
    "In Flight",       // SECT_FLYING
    "Road (Map)",
    "Wall (Map)",
    "Swamp",
    "Desert",
    "Bridge",
    "\n",
];

/// `genders[]` — SEX_x. NOTE: not `"\n"`-terminated in C.
pub static GENDERS: &[&str] = &[
    "Neutral", // SEX_NEUTRAL
    "Male",    // SEX_MALE
    "Female",  // SEX_FEMALE
];

/// `position_types[]` — POS_x positions (terminated with `"\n"`).
pub static POSITION_TYPES: &[&str] = &[
    "Dead",             // POS_DEAD
    "Mortally wounded", // POS_MORTALLYW
    "Incapacitated",    // POS_INCAP
    "Stunned",          // POS_STUNNED
    "Sleeping",         // POS_SLEEPING
    "Resting",          // POS_RESTING
    "Meditating",
    "Sitting",  // POS_SITTING
    "Fighting", // POS_FIGHTING
    "Standing", // POS_STANDING
    "\n",
];

/// `player_bits[]` — PLR_x player flags (terminated with `"\n"`).
pub static PLAYER_BITS: &[&str] = &[
    "KILLER",   // PLR_KILLER
    "THIEF",    // PLR_THIEF
    "FROZEN",   // PLR_FROZEN
    "DONTSET",  // PLR_DONTSET
    "WRITING",  // PLR_WRITING
    "MAILING",  // PLR_MAILING
    "CSH",      // PLR_CRASH
    "SITEOK",   // PLR_SITEOK
    "NOSHOUT",  // PLR_NOSHOUT
    "NOTITLE",  // PLR_NOTITLE
    "DELETED",  // PLR_DELETED
    "!UNUSED!", // PLR_LOADROOM (unused slot)
    "!WIZL",    // PLR_NOWIZLIST
    "!DEL",     // PLR_NODELETE
    "INVST",    // PLR_INVSTART
    "CRYO",     // PLR_CRYO
    "QUESTOR", "MULTIOK", "MBUILDER", "\n",
];

/// `action_bits[]` — MOB_x NPC action flags (terminated with `"\n"`).
pub static ACTION_BITS: &[&str] = &[
    "SPEC",         // MOB_SPEC
    "SENTINEL",     // MOB_SENTINEL
    "SCAVENGER",    // MOB_SCAVENGER
    "ISNPC",        // MOB_ISNPC
    "AWARE",        // MOB_AWARE
    "AGGR",         // MOB_AGGRESSIVE
    "STAY-ZONE",    // MOB_STAY_ZONE
    "WIMPY",        // MOB_WIMPY
    "AGGR_EVIL",    // MOB_AGGR_EVIL
    "AGGR_GOOD",    // MOB_AGGR_GOOD
    "AGGR_NEUTRAL", // MOB_AGGR_NEUTRAL
    "MEMORY",       // MOB_MEMORY
    "HELPER",       // MOB_HELPER
    "!CHARM",       // MOB_NOCHARM
    "!SUMMN",       // MOB_NOSUMMON
    "!SLEEP",       // MOB_NOSLEEP
    "!BASH",        // MOB_NOBASH
    "!BLIND",       // MOB_NOBLIND
    "QUESTMASTER",
    "QUESTMOB",
    "MOUNTABLE",
    "SPELLCASTER",
    "DBLATTACK",
    "!TALK",
    "MERCY",
    "DEITY",
    "PCHELPER_GOOD",
    "PCHELPER_NEUT",
    "PCHELPER_EVIL",
    "BANKER",
    "\n",
];

/// `preference_bits[]` — PRF_x player preference flags (terminated `"\n"`).
pub static PREFERENCE_BITS: &[&str] = &[
    "BRIEF",   // PRF_BRIEF
    "COMPACT", // PRF_COMPACT
    "DEAF",    // PRF_DEAF
    "!TELL",   // PRF_NOTELL
    "D_HP",    // PRF_DISPHP
    "D_MANA",  // PRF_DISPMANA
    "D_MOVE",  // PRF_DISPMOVE
    "AUTOEX",  // PRF_AUTOEXIT
    "!HASS",   // PRF_NOHASSLE
    "!ARENA",
    "SUMN",  // PRF_SUMMONABLE
    "!REP",  // PRF_NOREPEAT
    "LIGHT", // PRF_HOLYLIGHT
    "C1",    // PRF_COLOR_1
    "C2",    // PRF_COLOR_2
    "!WIZ",  // PRF_NOWIZ
    "L1",    // PRF_LOG1
    "L2",    // PRF_LOG2
    "!AUC",  // PRF_NOAUCT
    "!GOS",  // PRF_NOGOSS
    "!GTZ",  // PRF_NOGRATZ
    "RMFLG", // PRF_ROOMFLAGS
    "AFK",
    "AUTOSPLIT",
    "AUTOLOOT",
    "AUTOGOLD",
    "DISPEXP",
    "!TIC",
    "UNUSED",
    "L3",
    "!STACK",
    "\n",
];

/// `preference2_bits[]` — PRF2_x flags. The C table is padded with many `"\n"`
/// entries after the real flags; preserved verbatim.
pub static PREFERENCE2_BITS: &[&str] = &[
    "QUEST",
    "LOCKOUT",
    "!MAP",
    "CTITLE",
    "CLAN_TALK",
    "D_MOB",
    "MBUILDING",
    "MERCY",
    "ADVCANCEDMAP",
    "INTANGIBLE",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n",
    "\n",
];

/// `affected_bits[]` — AFF_x affect flags (terminated with `"\n"`).
pub static AFFECTED_BITS: &[&str] = &[
    "BLIND",        // AFF_BLIND
    "INVIS",        // AFF_INVISIBLE
    "DETECT-ALIGN", // AFF_DETECT_ALIGN
    "DETECT-INVIS", // AFF_DETECT_INVIS
    "DETECT-MAGIC", // AFF_DETECT_MAGIC
    "SENSE-LIFE",   // AFF_SENSE_LIFE
    "WATERWALK",    // AFF_WATERWALK
    "SANCTUARY",    // AFF_SANCTUARY
    "GROUP",        // AFF_GROUP
    "CURSE",        // AFF_CURSE
    "INFRAVISION",  // AFF_INFRAVISION
    "POISON",       // AFF_POISON
    "UNUSED",       // AFF_PROTECT_EVIL (unused)
    "UNUSED",       // AFF_PROTECT_GOOD (unused)
    "SLEEP",        // AFF_SLEEP
    "!TRACK",       // AFF_NOTRACK
    "TAMED",
    "CONVERGENCE",
    "SNEAK", // AFF_SNEAK
    "HIDE",  // AFF_HIDE
    "AUTUS",
    "CHARM", // AFF_CHARM
    "!PORTAL",
    "PLAGUED",
    "CHAINED",
    "REDIRECT CHARGE",
    "CHARGED",
    "\n",
];

/// `connected_types[]` — CON_x connection states (terminated with `"\n"`).
pub static CONNECTED_TYPES: &[&str] = &[
    "Playing",        // CON_PLAYING
    "Disconnecting",  // CON_CLOSE
    "Get name",       // CON_GET_NAME
    "Confirm name",   // CON_NAME_CNFRM
    "Get password",   // CON_PASSWORD
    "Get new PW",     // CON_NEWPASSWD
    "Confirm new PW", // CON_CNFPASSWD
    "Select sex",     // CON_QSEX
    "Select class",   // CON_QCLASS
    "Reading MOTD",   // CON_RMOTD
    "Main Menu",      // CON_MENU
    "Get descript.",  // CON_EXDESC
    "Changing PW 1",  // CON_CHPWD_GETOLD
    "Changing PW 2",  // CON_CHPWD_GETNEW
    "Changing PW 3",  // CON_CHPWD_VRFY
    "Self-Delete 1",  // CON_DELCNF1
    "Self-Delete 2",  // CON_DELCNF2
    "Object editor",  // CON_OEDIT
    "Room editor",    // CON_REDIT
    "Zone editor",    // CON_ZEDIT
    "Mobile editor",  // CON_MEDIT
    "Shop editor",    // CON_SEDIT
    "ANSI query",
    "Select race",
    "Roll stats",
    "Select town",
    "Trigger editor",
    "Newbie start",
    "Help editor",
    "Action editor",
    "Text editor",
    "Select deity",
    "\n",
];

/// `where[]` — WEAR_x labels for the equipment list (`eq` command). Contains
/// embedded color codes (`&b`, `&n`). NOTE: not `"\n"`-terminated in C.
pub static WHERE: &[&str] = &[
    "&b<&nused as light&b>&n      ",
    "&b<&nworn on finger&b>&n     ",
    "&b<&nworn on finger&b>&n     ",
    "&b<&nworn around neck&b>&n   ",
    "&b<&nworn around neck&b>&n   ",
    "&b<&nworn on body&b>&n       ",
    "&b<&nworn on head&b>&n       ",
    "&b<&nworn on legs&b>&n       ",
    "&b<&nworn on feet&b>&n       ",
    "&b<&nworn on hands&b>&n      ",
    "&b<&nworn on arms&b>&n       ",
    "&b<&nworn as shield&b>&n     ",
    "&b<&nworn about body&b>&n    ",
    "&b<&nworn about waist&b>&n   ",
    "&b<&nworn around wrist&b>&n  ",
    "&b<&nworn around wrist&b>&n  ",
    "&b<&nwielded&b>&n            ",
    "&b<&nheld&b>&n               ",
    "&b<&nworn on shoulders&b>&n  ",
    "&b<&nworn around ankle&b>&n  ",
    "&b<&nworn around ankle&b>&n  ",
    "&b<&nworn on face&b>&n       ",
];

/// `equipment_types[]` — WEAR_x labels for the `stat` command (terminated
/// with `"\n"`).
pub static EQUIPMENT_TYPES: &[&str] = &[
    "Used as light",
    "Worn on right finger",
    "Worn on left finger",
    "First worn around Neck",
    "Second worn around Neck",
    "Worn on body",
    "Worn on head",
    "Worn on legs",
    "Worn on feet",
    "Worn on hands",
    "Worn on arms",
    "Worn as shield",
    "Worn about body",
    "Worn around waist",
    "Worn around right wrist",
    "Worn around left wrist",
    "Wielded",
    "Held",
    "Worn on shoulders",
    "Worn around right ankle",
    "Worn around left ankle",
    "Worn over face",
    "\n",
];

/// `item_types[]` — ITEM_x object types (terminated with `"\n"`).
pub static ITEM_TYPES: &[&str] = &[
    "UNDEFINED",     // 0
    "LIGHT",         // ITEM_LIGHT
    "SCROLL",        // ITEM_SCROLL
    "WAND",          // ITEM_WAND
    "STAFF",         // ITEM_STAFF
    "WEAPON",        // ITEM_WEAPON
    "FIRE WEAPON",   // ITEM_FIREWEAPON
    "MISSILE",       // ITEM_MISSILE
    "TREASURE",      // ITEM_TREASURE
    "ARMOR",         // ITEM_ARMOR
    "POTION",        // ITEM_POTION
    "WORN",          // ITEM_WORN
    "OTHER",         // ITEM_OTHER
    "TRASH",         // ITEM_TRASH
    "TRAP",          // ITEM_TRAP
    "CONTAINER",     // ITEM_CONTAINER
    "NOTE",          // ITEM_NOTE
    "LIQ CONTAINER", // ITEM_DRINKCON
    "KEY",           // ITEM_KEY
    "FOOD",          // ITEM_FOOD
    "MONEY",         // ITEM_MONEY
    "PEN",           // ITEM_PEN
    "BOAT",          // ITEM_BOAT
    "FOUNTAIN",      // ITEM_FOUNTAIN
    "PORTAL",
    "HIT_REGEN",
    "MANA_REGEN",
    "MOVE_REGEN",
    "ATM",
    "\n",
];

/// `wear_bits[]` — ITEM_WEAR_x wear-location bitvector (terminated `"\n"`).
pub static WEAR_BITS: &[&str] = &[
    "TAKE",   // ITEM_WEAR_TAKE
    "FINGER", // ITEM_WEAR_FINGER
    "NECK",   // ITEM_WEAR_NECK
    "BODY",   // ITEM_WEAR_BODY
    "HEAD",   // ITEM_WEAR_HEAD
    "LEGS",   // ITEM_WEAR_LEGS
    "FEET",   // ITEM_WEAR_FEET
    "HANDS",  // ITEM_WEAR_HANDS
    "ARMS",   // ITEM_WEAR_ARMS
    "SHIELD", // ITEM_WEAR_SHIELD
    "ABOUT",  // ITEM_WEAR_ABOUT
    "WAIST",  // ITEM_WEAR_WAIST
    "WRIST",  // ITEM_WEAR_WRIST
    "WIELD",  // ITEM_WEAR_WIELD
    "HOLD",   // ITEM_WEAR_HOLD
    "SHOULDERS",
    "ANKLE",
    "FACE",
    "\n",
];

/// `extra_bits[]` — ITEM_x extra-flag bitvector (terminated with `"\n"`).
pub static EXTRA_BITS: &[&str] = &[
    "GLOW",      // ITEM_GLOW
    "HUM",       // ITEM_HUM
    "!RENT",     // ITEM_NORENT
    "!DONATE",   // ITEM_NODONATE
    "!INVIS",    // ITEM_NOINVIS
    "INVISIBLE", // ITEM_INVISIBLE
    "MAGIC",     // ITEM_MAGIC
    "!DROP",     // ITEM_NODROP
    "BLESS",     // ITEM_BLESS
    "!GOOD",     // ITEM_ANTI_GOOD
    "!EVIL",     // ITEM_ANTI_EVIL
    "!NEUTRAL",  // ITEM_ANTI_NEUTRAL
    "!MAGE",     // ITEM_ANTI_MAGE
    "!CLERIC",   // ITEM_ANTI_CLERIC
    "!THIEF",    // ITEM_ANTI_THIEF
    "!WARRIOR",  // ITEM_ANTI_WARRIOR
    "!SELL",     // ITEM_NOSELL
    "!ARTISAN",
    "\n",
];

/// `apply_types[]` — APPLY_x stat-modifier types (terminated with `"\n"`).
pub static APPLY_TYPES: &[&str] = &[
    "NONE",        // APPLY_NONE
    "STR",         // APPLY_STR
    "DEX",         // APPLY_DEX
    "INT",         // APPLY_INT
    "WIS",         // APPLY_WIS
    "CON",         // APPLY_CON
    "CHA",         // APPLY_CHA
    "CLASS",       // APPLY_CLASS
    "LEVEL",       // APPLY_LEVEL
    "AGE",         // APPLY_AGE
    "CHAR_WEIGHT", // APPLY_CHAR_WEIGHT
    "CHAR_HEIGHT", // APPLY_CHAR_HEIGHT
    "MAXMANA",     // APPLY_MANA
    "MAXHIT",      // APPLY_HIT
    "MAXMOVE",     // APPLY_MOVE
    "GOLD",        // APPLY_GOLD
    "EXP",         // APPLY_EXP
    "DEFENSE",     // APPLY_AC
    "MDEFENSE",
    "POWER",
    "MPOWER",
    "TECHNIQUE",
    "DAMAGE", // APPLY_DAMROLL -- "This is BOGUS, don't use it." (per C)
    "\n",
];

/// `container_bits[]` — CONT_x container flags (terminated with `"\n"`).
pub static CONTAINER_BITS: &[&str] = &[
    "CLOSEABLE", // CONT_CLOSEABLE
    "PICKPROOF", // CONT_PICKPROOF
    "CLOSED",    // CONT_CLOSED
    "LOCKED",    // CONT_LOCKED
    "\n",
];

/// `drinks[]` — LIQ_x full liquid names (terminated with `"\n"`).
pub static DRINKS: &[&str] = &[
    "water",            // LIQ_WATER
    "beer",             // LIQ_BEER
    "wine",             // LIQ_WINE
    "ale",              // LIQ_ALE
    "dark ale",         // LIQ_DARKALE
    "whisky",           // LIQ_WHISKY
    "lemonade",         // LIQ_LEMONADE
    "firebreather",     // LIQ_FIREBRT
    "local speciality", // LIQ_LOCALSPC
    "slime mold juice", // LIQ_SLIME
    "milk",             // LIQ_MILK
    "tea",              // LIQ_TEA
    "coffee",           // LIQ_COFFE
    "blood",            // LIQ_BLOOD
    "salt water",       // LIQ_SALTWATER
    "clear water",      // LIQ_CLEARWATER
    "\n",
];

/// `drinknames[]` — one-word alias for each LIQ_x drink (terminated `"\n"`).
pub static DRINKNAMES: &[&str] = &[
    "water",
    "beer",
    "wine",
    "ale",
    "ale",
    "whisky",
    "lemonade",
    "firebreather",
    "local",
    "juice",
    "milk",
    "tea",
    "coffee",
    "blood",
    "salt",
    "water",
    "\n",
];

/// `drink_aff[][3]` — per-LIQ_x effect on {drunkenness, fullness, thirst}
/// (see values.doc). One inner triple per drink, no sentinel.
pub static DRINK_AFF: &[[i32; 3]] = &[
    [0, 1, 10],
    [3, 2, 5],
    [5, 2, 5],
    [2, 2, 5],
    [1, 2, 5],
    [6, 1, 4],
    [0, 1, 8],
    [10, 0, 0],
    [3, 3, 3],
    [0, 4, -8],
    [0, 3, 6],
    [0, 1, 6],
    [-1, 1, 6],
    [0, 2, -1],
    [0, 1, -2],
    [0, 0, 13],
];

/// `color_liquid[]` — color of each LIQ_x drink. No `"\n"` sentinel in C.
pub static COLOR_LIQUID: &[&str] = &[
    "clear",
    "brown",
    "clear",
    "brown",
    "dark",
    "golden",
    "red",
    "green",
    "clear",
    "light green",
    "white",
    "brown",
    "black",
    "red",
    "clear",
    "crystal clear",
];

/// `fullness[]` — fullness label for drink containers. No `"\n"` sentinel.
pub static FULLNESS: &[&str] = &["less than half ", "about half ", "more than half ", ""];

/// `spell_wear_off_msg[]` — messages shown when a spell affect wears off,
/// indexed by spell number (slot 0 is the reserved DB.C placeholder). No
/// `"\n"` sentinel in C.
pub static SPELL_WEAR_OFF_MSG: &[&str] = &[
    "RESERVED DB.C",            // 0
    "You feel less protected.", // 1
    "!Teleport!",
    "You feel less righteous.",
    "Your vision returns.",
    "You feel more self-confident.",
    "!Clone!",
    "!Control Weather!",
    "!Create Food!",
    "!Create Water!",
    "!Cure Blind!",
    "!Cure Critic!",
    "!Cure Light!",
    "You feel more optimistic.",
    "You feel less aware.",
    "Your eyes stop tingling.",
    "The detect magic wears off.",
    "The detect poison wears off.",
    "!Earthquake!",
    "!Enchant Weapon!",
    "!Heal!",
    "You feel yourself exposed.",
    "!Locate object!",
    "You feel less sick.",
    "!Remove Curse!",
    "The white aura around your body fades.",
    "You feel less tired.",
    "You feel weaker.",
    "!Summon!",
    "!Word of Recall!",
    "!Remove Poison!",
    "You feel less aware of your surroundings.",
    "!Animate Dead!",
    "!Group Armor!",
    "!Group Heal!",
    "!Group Recall!",
    "Your night vision seems to fade.",
    "Your feet seem less boyant.",
    "Your skin slowly softens as it returns to normal.",
    "You regain your nerves.",
    "!Recharge!",
    "!Portal!",
    "!Group Stone Skin!",
    "!Locate Target!",
    "Your focus of magic power diminishes back to normal.",
    "Your ability to conserve mana use has worn off.",
    "You feel that you may now be affected by portals.",
    "!Regen Mana!",
    "!Home!",
    "!Word of Retreat!",
    "You are liberated of your chains!",
    "The &Kglow&n around your body fades.",
];

/// `spell_affect_msg[]` — messages shown when a spell affect is applied,
/// indexed by spell number (slot 0 reserved). No `"\n"` sentinel in C.
pub static SPELL_AFFECT_MSG: &[&str] = &[
    "RESERVED DB.C",                    // 0
    "You feel someone protecting you.", // 1
    "!Teleport!",
    "You feel righteous.",
    "You have been blinded!",
    "You feel very uncomfortable.",
    "!Clone!",
    "!Control Weather!",
    "!Create Food!",
    "!Create Water!",
    "!Cure Blind!",
    "!Cure Critic!",
    "!Cure Light!",
    "You feel very uncomfortable.",
    "Your eyes tingle.",
    "Your eyes tingle.",
    "Your eyes tingle.",
    "Your eyes tingle.",
    "!Earthquake!",
    "!Enchant Weapon!",
    "!Heal!",
    "You vanish.",
    "!Locate object!",
    "You feel very sick.",
    "!Remove Curse!",
    "A white aura momentarily surrounds you.",
    "You feel very sleepy...  Zzzz......",
    "You feel stronger!",
    "!Summon!",
    "!Word of Recall!",
    "!Remove Poison!",
    "Your feel your awareness improve.",
    "!Animate Dead!",
    "!Group Armor!",
    "!Group Heal!",
    "!Group Recall!",
    "Your eyes glow red.",
    "Your feel webbing between your toes.",
    "Your skin turns to stone!.",
    "!Fear!",
    "!Recharge!",
    "!Portal!",
    "!Group Stone Skin!",
    "!Locate Target!",
    "A red aura momentarily surrounds you.",
    "A green aura momentarily surrounds you.",
    "You feel a protection from the heavens.",
    "!Regen Mana!",
    "!Home!",
    "!Word of Retreat!",
    "Your feet are suddenly chained together!",
    "You feel electrified and &Kglow&n!",
];

/// `affect_wear_off_msg[]` — AFF_x-only wear-off messages (permanent-item
/// affects), indexed by AFF bit. No `"\n"` sentinel in C.
pub static AFFECT_WEAR_OFF_MSG: &[&str] = &[
    "Your vision returns.",
    "You feel yourself exposed.",
    "You feel less aware.",
    "Your eyes stop tingling.",
    "The detect magic wears off.",
    "You feel less aware of your surroundings.",
    "Your feet seem less boyant.",
    "The white aura around your body fades.",
    "!Group!",
    "You feel more optimistic.",
    "Your night vision seems to fade.",
    "You feel less sick.",
    "!UNUSED!",
    "!UNUSED!",
    "You feel less tired.",
    "Your feet settle back to the ground.",
    "You feel the wild inside of you... ROAR!",
    "Your focus of magic power diminishes back to normal.",
    "You feel less sneaky.",
    "You feel yourself exposed.",
    "Your ability to conserve mana use has worn off.",
    "You feel more self-confident.",
    "You feel that you may now be affected by portals.",
    "You suddenly feel VERY healthy!",
    "You are liberated of your chains!",
    "The &Kglow&n around you fades.",
    "You can no longer maintain your control over the lightning charge and it &RSURGES&n through your body!",
];

/// `affect_aff_msg[]` — AFF_x-only apply messages (permanent-item affects),
/// indexed by AFF bit. No `"\n"` sentinel in C.
pub static AFFECT_AFF_MSG: &[&str] = &[
    "You have been blinded!",
    "You vanish.",
    "Your eyes tingle.",
    "Your eyes tingle.",
    "Your eyes tingle.",
    "Your feel your awareness improve.",
    "Your feel webbing between your toes.",
    "A white aura momentarily surrounds you.",
    "!Group!",
    "You feel very uncomfortable.",
    "Your eyes glow red.",
    "You feel very sick.",
    "!UNUSED!",
    "!UNUSED!",
    "You feel very sleepy...  Zzzz......",
    "Your feet begin to glide slightly above the ground.",
    "You feel very tame.",
    "A red aura momentarily surrounds you.",
    "You feel very sneaky.",
    "You are enshrouded!",
    "A green aura momentarily surrounds you.",
    "You feel very uncomfortable.",
    "You feel a protection from the heavens.",
    "You become violently &eill&n.",
    "Your feet are suddenly chained together!",
    "You feel electrified and &Kglow&n!",
    "Your hair stands on end and you feel &KELECTRIFIED&n!",
];

/// `npc_class_types[]` — NPC class names (terminated with `"\n"`).
/// (PC class tables `class_abbrevs[]` / `pc_class_types[]` live in `class.c`,
/// not in `constants.c`.)
pub static NPC_CLASS_TYPES: &[&str] = &["Normal", "Undead", "\n"];

/// `rev_dir[]` — reverse-direction index for each direction in [`DIRS`].
/// In C the source embeds a `#if defined(OASIS_MPROG)` block inside the array
/// literal; `OASIS_MPROG` is **not** defined in this build, so the effective
/// values are `{north->2, east->3, south->0, west->1, up->5, down->4}`.
pub static REV_DIR: &[i32] = &[2, 3, 0, 1, 5, 4];

/// `movement_loss[]` — base movement-point cost per SECT_x sector. No sentinel.
pub static MOVEMENT_LOSS: &[i32] = &[
    1, // Inside
    1, // City
    2, // Field
    3, // Forest
    4, // Hills
    6, // Mountains
    4, // Swimming
    1, // Unswimable
    1, // Flying
    5, // Underwater
    3, // Road
    1, // Wall
    5, // Swamp
    6, // Desert
    3, // Bridge
];

/// `special_movement_loss[]` — special (e.g. flying/mounted) movement cost per
/// SECT_x sector. No sentinel.
pub static SPECIAL_MOVEMENT_LOSS: &[i32] = &[
    10, // Inside
    10, // City
    20, // Field
    30, // Forest
    40, // Hills
    60, // Mountains
    40, // Swimming
    10, // Unswimable
    10, // Flying
    50, // Underwater
    15, // Road
    10, // Wall
    50, // Swamp
    60, // Desert
    30, // Bridge
];

/// `weekdays[]` — in-game day-of-week names. No `"\n"` sentinel in C.
pub static WEEKDAYS: &[&str] = &[
    "the Day of the Moon",
    "the Day of the Great Disaster",
    "the Day of the Deception",
    "the Day of Thunder",
    "the Day of Freedom",
    "the Day of the Great Gods",
    "the Day of the Sun",
];

/// `month_name[]` — in-game month names. No `"\n"` sentinel in C.
pub static MONTH_NAME: &[&str] = &[
    "Month of Winter", // 0
    "Month of the Winter Wolf",
    "Month of the Frost Giant",
    "Month of the Old Forces",
    "Month of the Grand Struggle",
    "Month of the Spring",
    "Month of Nature",
    "Month of Futility",
    "Month of the Dragon",
    "Month of the Sun",
    "Month of the Heat",
    "Month of the Battle",
    "Month of the Dark Shades",
    "Month of the Shadows",
    "Month of the Long Shadows",
    "Month of the Ancient Darkness",
    "Month of the Great Evil",
];

/// `citizen_titles[7]` — town-citizen title triples `{male, female, neutral}`,
/// indexed by citizen rank (Peasant .. King).
pub static CITIZEN_TITLES: &[[&str; 3]] = &[
    ["Peasant ", "Peasant ", "Peasant "],
    ["Squire ", "Squire ", "Squire "],
    ["Knight ", "Knight ", "Knight "],
    ["Nobleman ", "Lady ", "Noble "],
    ["Duke ", "Duchess ", "Duke "],
    ["Master ", "Mistress ", "Master "],
    ["King ", "Queen ", "King "],
];

/// `citizen_colors[]` — color code per citizen rank. In C these are the
/// `KBCYN`/`KBGRN` macros from `screen.h`, which expand to `"&c"` / `"&g"`.
pub static CITIZEN_COLORS: &[&str] = &[
    "&c", // KBCYN
    "&c", // KBCYN
    "&c", // KBCYN
    "&c", // KBCYN
    "&c", // KBCYN
    "&c", // KBCYN
    "&g", // KBGRN
];

// ---------------------------------------------------------------------------
// Ability-score "app" bonus tables (constants.c).
//
// These mirror the CircleMUD/DeltaMUD `*_app[]` arrays 1:1 in C field order.
// Index convention: every table except `STR_APP` is indexed directly by the
// 0..=25 ability score (`GET_DEX`/`GET_CON`/`GET_INT`/`GET_WIS`). `STR_APP`
// is indexed by [`strength_apply_index`] which folds the percentile
// `str_add` (18/01..18/100) into entries 26..=30 — see the helper below.
//
// NOTE on DeltaMUD divergence: combat to-hit/damage do NOT consume these
// tables here. DeltaMUD's `chance()`/`dam_multi()` (utils.c) use the raw
// ability scores and the technique/power combat ratings directly, so the
// stock CircleMUD `str_app.tohit/.todam` columns and the `dex_app.defensive`
// AC adjustment simply do not exist / are never indexed in this codebase.
// `DEX_APP` is provided for fidelity but, like the C `dex_app[]`, has no live
// consumer (declared `extern` in fight.c, never indexed anywhere).
// ---------------------------------------------------------------------------

/// `struct str_app_type` (structs.h). DeltaMUD's str_app only carries the
/// carry/wield weight columns (no tohit/todam columns).
pub struct StrAppType {
    /// Maximum weight that can be carried.
    pub carry_w: i32,
    /// Maximum weight that can be wielded.
    pub wield_w: i32,
}

/// `str_app[]` (constants.c). Indexed by [`strength_apply_index`], NOT by the
/// raw strength score: entries 0..=25 are STR 0..25; entries 26..=30 encode
/// 18/01-50, 18/51-75, 18/76-90, 18/91-99, 18/100.
pub static STR_APP: &[StrAppType] = &[
    StrAppType {
        carry_w: 0,
        wield_w: 0,
    }, // str = 0
    StrAppType {
        carry_w: 3,
        wield_w: 1,
    }, // str = 1
    StrAppType {
        carry_w: 3,
        wield_w: 2,
    },
    StrAppType {
        carry_w: 10,
        wield_w: 3,
    },
    StrAppType {
        carry_w: 25,
        wield_w: 4,
    },
    StrAppType {
        carry_w: 55,
        wield_w: 5,
    }, // str = 5
    StrAppType {
        carry_w: 80,
        wield_w: 6,
    },
    StrAppType {
        carry_w: 90,
        wield_w: 7,
    },
    StrAppType {
        carry_w: 100,
        wield_w: 8,
    },
    StrAppType {
        carry_w: 100,
        wield_w: 9,
    },
    StrAppType {
        carry_w: 115,
        wield_w: 10,
    }, // str = 10
    StrAppType {
        carry_w: 115,
        wield_w: 11,
    },
    StrAppType {
        carry_w: 140,
        wield_w: 12,
    },
    StrAppType {
        carry_w: 140,
        wield_w: 13,
    },
    StrAppType {
        carry_w: 170,
        wield_w: 14,
    },
    StrAppType {
        carry_w: 170,
        wield_w: 15,
    }, // str = 15
    StrAppType {
        carry_w: 195,
        wield_w: 16,
    },
    StrAppType {
        carry_w: 220,
        wield_w: 18,
    },
    StrAppType {
        carry_w: 255,
        wield_w: 20,
    }, // str = 18
    StrAppType {
        carry_w: 640,
        wield_w: 40,
    },
    StrAppType {
        carry_w: 700,
        wield_w: 40,
    }, // str = 20
    StrAppType {
        carry_w: 810,
        wield_w: 40,
    },
    StrAppType {
        carry_w: 970,
        wield_w: 40,
    },
    StrAppType {
        carry_w: 1130,
        wield_w: 40,
    },
    StrAppType {
        carry_w: 1440,
        wield_w: 40,
    },
    StrAppType {
        carry_w: 1750,
        wield_w: 40,
    }, // str = 25
    StrAppType {
        carry_w: 280,
        wield_w: 22,
    }, // str = 18/0  - 18/50
    StrAppType {
        carry_w: 305,
        wield_w: 24,
    }, // str = 18/51 - 18/75
    StrAppType {
        carry_w: 330,
        wield_w: 26,
    }, // str = 18/76 - 18/90
    StrAppType {
        carry_w: 380,
        wield_w: 28,
    }, // str = 18/91 - 18/99
    StrAppType {
        carry_w: 480,
        wield_w: 30,
    }, // str = 18/100
];

/// `STRENGTH_APPLY_INDEX(ch)` (utils.h). Folds `str_add` (18/01..18/100) into
/// the percentile entries 26..=30 of [`STR_APP`]; otherwise the raw strength.
/// Pass `str_` and `str_add` from `aff_abils`.
#[inline]
pub fn strength_apply_index(str_: i32, str_add: i32) -> usize {
    if str_add == 0 || str_ != 18 {
        str_.clamp(0, (STR_APP.len() - 1) as i32) as usize
    } else if str_add <= 50 {
        26
    } else if str_add <= 75 {
        27
    } else if str_add <= 90 {
        28
    } else if str_add <= 99 {
        29
    } else {
        30
    }
}

/// `struct dex_skill_type` (structs.h) — thief-skill bonuses by dexterity.
pub struct DexSkillType {
    pub p_pocket: i32,
    pub p_locks: i32,
    pub traps: i32,
    pub sneak: i32,
    pub hide: i32,
}

/// `dex_app_skill[]` (constants.c). Indexed directly by dexterity 0..=25.
pub static DEX_APP_SKILL: &[DexSkillType] = &[
    DexSkillType {
        p_pocket: -99,
        p_locks: -99,
        traps: -90,
        sneak: -99,
        hide: -60,
    }, // dex = 0
    DexSkillType {
        p_pocket: -90,
        p_locks: -90,
        traps: -60,
        sneak: -90,
        hide: -50,
    }, // dex = 1
    DexSkillType {
        p_pocket: -80,
        p_locks: -80,
        traps: -40,
        sneak: -80,
        hide: -45,
    },
    DexSkillType {
        p_pocket: -70,
        p_locks: -70,
        traps: -30,
        sneak: -70,
        hide: -40,
    },
    DexSkillType {
        p_pocket: -60,
        p_locks: -60,
        traps: -30,
        sneak: -60,
        hide: -35,
    },
    DexSkillType {
        p_pocket: -50,
        p_locks: -50,
        traps: -20,
        sneak: -50,
        hide: -30,
    }, // dex = 5
    DexSkillType {
        p_pocket: -40,
        p_locks: -40,
        traps: -20,
        sneak: -40,
        hide: -25,
    },
    DexSkillType {
        p_pocket: -30,
        p_locks: -30,
        traps: -15,
        sneak: -30,
        hide: -20,
    },
    DexSkillType {
        p_pocket: -20,
        p_locks: -20,
        traps: -15,
        sneak: -20,
        hide: -15,
    },
    DexSkillType {
        p_pocket: -15,
        p_locks: -10,
        traps: -10,
        sneak: -20,
        hide: -10,
    },
    DexSkillType {
        p_pocket: -10,
        p_locks: -5,
        traps: -10,
        sneak: -15,
        hide: -5,
    }, // dex = 10
    DexSkillType {
        p_pocket: -5,
        p_locks: 0,
        traps: -5,
        sneak: -10,
        hide: 0,
    },
    DexSkillType {
        p_pocket: 0,
        p_locks: 0,
        traps: 0,
        sneak: -5,
        hide: 0,
    },
    DexSkillType {
        p_pocket: 0,
        p_locks: 0,
        traps: 0,
        sneak: 0,
        hide: 0,
    },
    DexSkillType {
        p_pocket: 0,
        p_locks: 0,
        traps: 0,
        sneak: 0,
        hide: 0,
    },
    DexSkillType {
        p_pocket: 0,
        p_locks: 0,
        traps: 0,
        sneak: 0,
        hide: 0,
    }, // dex = 15
    DexSkillType {
        p_pocket: 0,
        p_locks: 5,
        traps: 0,
        sneak: 0,
        hide: 0,
    },
    DexSkillType {
        p_pocket: 5,
        p_locks: 10,
        traps: 0,
        sneak: 5,
        hide: 5,
    },
    DexSkillType {
        p_pocket: 10,
        p_locks: 15,
        traps: 5,
        sneak: 10,
        hide: 10,
    }, // dex = 18
    DexSkillType {
        p_pocket: 15,
        p_locks: 20,
        traps: 10,
        sneak: 15,
        hide: 15,
    },
    DexSkillType {
        p_pocket: 15,
        p_locks: 20,
        traps: 10,
        sneak: 15,
        hide: 15,
    }, // dex = 20
    DexSkillType {
        p_pocket: 20,
        p_locks: 25,
        traps: 10,
        sneak: 15,
        hide: 20,
    },
    DexSkillType {
        p_pocket: 20,
        p_locks: 25,
        traps: 15,
        sneak: 20,
        hide: 20,
    },
    DexSkillType {
        p_pocket: 25,
        p_locks: 25,
        traps: 15,
        sneak: 20,
        hide: 20,
    },
    DexSkillType {
        p_pocket: 25,
        p_locks: 30,
        traps: 15,
        sneak: 25,
        hide: 25,
    },
    DexSkillType {
        p_pocket: 25,
        p_locks: 30,
        traps: 15,
        sneak: 25,
        hide: 25,
    }, // dex = 25
];

/// `struct dex_app_type` (structs.h). Provided for fidelity; DeltaMUD never
/// indexes `dex_app[]` (no live consumer).
pub struct DexAppType {
    pub reaction: i32,
    pub miss_att: i32,
    pub defensive: i32,
}

/// `dex_app[]` (constants.c). Indexed directly by dexterity 0..=25.
pub static DEX_APP: &[DexAppType] = &[
    DexAppType {
        reaction: -7,
        miss_att: -7,
        defensive: 6,
    }, // dex = 0
    DexAppType {
        reaction: -6,
        miss_att: -6,
        defensive: 5,
    }, // dex = 1
    DexAppType {
        reaction: -4,
        miss_att: -4,
        defensive: 5,
    },
    DexAppType {
        reaction: -3,
        miss_att: -3,
        defensive: 4,
    },
    DexAppType {
        reaction: -2,
        miss_att: -2,
        defensive: 3,
    },
    DexAppType {
        reaction: -1,
        miss_att: -1,
        defensive: 2,
    }, // dex = 5
    DexAppType {
        reaction: 0,
        miss_att: 0,
        defensive: 1,
    },
    DexAppType {
        reaction: 0,
        miss_att: 0,
        defensive: 0,
    },
    DexAppType {
        reaction: 0,
        miss_att: 0,
        defensive: 0,
    },
    DexAppType {
        reaction: 0,
        miss_att: 0,
        defensive: 0,
    },
    DexAppType {
        reaction: 0,
        miss_att: 0,
        defensive: 0,
    }, // dex = 10
    DexAppType {
        reaction: 0,
        miss_att: 0,
        defensive: 0,
    },
    DexAppType {
        reaction: 0,
        miss_att: 0,
        defensive: 0,
    },
    DexAppType {
        reaction: 0,
        miss_att: 0,
        defensive: 0,
    },
    DexAppType {
        reaction: 0,
        miss_att: 0,
        defensive: 0,
    },
    DexAppType {
        reaction: 0,
        miss_att: 0,
        defensive: -1,
    }, // dex = 15
    DexAppType {
        reaction: 1,
        miss_att: 1,
        defensive: -2,
    },
    DexAppType {
        reaction: 2,
        miss_att: 2,
        defensive: -3,
    },
    DexAppType {
        reaction: 2,
        miss_att: 2,
        defensive: -4,
    }, // dex = 18
    DexAppType {
        reaction: 3,
        miss_att: 3,
        defensive: -4,
    },
    DexAppType {
        reaction: 3,
        miss_att: 3,
        defensive: -4,
    }, // dex = 20
    DexAppType {
        reaction: 4,
        miss_att: 4,
        defensive: -5,
    },
    DexAppType {
        reaction: 4,
        miss_att: 4,
        defensive: -5,
    },
    DexAppType {
        reaction: 4,
        miss_att: 4,
        defensive: -5,
    },
    DexAppType {
        reaction: 5,
        miss_att: 5,
        defensive: -6,
    },
    DexAppType {
        reaction: 5,
        miss_att: 5,
        defensive: -6,
    }, // dex = 25
];

/// `struct con_app_type` (structs.h).
pub struct ConAppType {
    /// Bonus hitpoints per level (used in `advance_level`).
    pub hitp: i32,
    /// Survival % when raising from death (shock).
    pub shock: i32,
}

/// `con_app[]` (constants.c). Indexed directly by constitution 0..=25.
pub static CON_APP: &[ConAppType] = &[
    ConAppType {
        hitp: -4,
        shock: 20,
    }, // con = 0
    ConAppType {
        hitp: -3,
        shock: 25,
    }, // con = 1
    ConAppType {
        hitp: -2,
        shock: 30,
    },
    ConAppType {
        hitp: -2,
        shock: 35,
    },
    ConAppType {
        hitp: -1,
        shock: 40,
    },
    ConAppType {
        hitp: -1,
        shock: 45,
    }, // con = 5
    ConAppType {
        hitp: -1,
        shock: 50,
    },
    ConAppType { hitp: 0, shock: 55 },
    ConAppType { hitp: 0, shock: 60 },
    ConAppType { hitp: 0, shock: 65 },
    ConAppType { hitp: 0, shock: 70 }, // con = 10
    ConAppType { hitp: 0, shock: 75 },
    ConAppType { hitp: 0, shock: 80 },
    ConAppType { hitp: 0, shock: 85 },
    ConAppType { hitp: 0, shock: 88 },
    ConAppType { hitp: 1, shock: 90 }, // con = 15
    ConAppType { hitp: 2, shock: 95 },
    ConAppType { hitp: 2, shock: 97 },
    ConAppType { hitp: 3, shock: 99 }, // con = 18
    ConAppType { hitp: 3, shock: 99 },
    ConAppType { hitp: 4, shock: 99 }, // con = 20
    ConAppType { hitp: 5, shock: 99 },
    ConAppType { hitp: 5, shock: 99 },
    ConAppType { hitp: 5, shock: 99 },
    ConAppType { hitp: 6, shock: 99 },
    ConAppType { hitp: 6, shock: 99 }, // con = 25
];

/// `struct int_app_type` (structs.h).
pub struct IntAppType {
    /// % a player learns a spell/skill per practice.
    pub learn: i32,
}

/// `int_app[]` (constants.c). Indexed directly by intelligence 0..=25.
pub static INT_APP: &[IntAppType] = &[
    IntAppType { learn: 3 }, // int = 0
    IntAppType { learn: 5 }, // int = 1
    IntAppType { learn: 7 },
    IntAppType { learn: 8 },
    IntAppType { learn: 9 },
    IntAppType { learn: 10 }, // int = 5
    IntAppType { learn: 11 },
    IntAppType { learn: 12 },
    IntAppType { learn: 13 },
    IntAppType { learn: 15 },
    IntAppType { learn: 17 }, // int = 10
    IntAppType { learn: 19 },
    IntAppType { learn: 22 },
    IntAppType { learn: 25 },
    IntAppType { learn: 30 },
    IntAppType { learn: 35 }, // int = 15
    IntAppType { learn: 40 },
    IntAppType { learn: 45 },
    IntAppType { learn: 50 }, // int = 18
    IntAppType { learn: 53 },
    IntAppType { learn: 55 }, // int = 20
    IntAppType { learn: 56 },
    IntAppType { learn: 57 },
    IntAppType { learn: 58 },
    IntAppType { learn: 59 },
    IntAppType { learn: 60 }, // int = 25
];

/// `struct wis_app_type` (structs.h).
pub struct WisAppType {
    /// Practices gained per level (used in `advance_level`).
    pub bonus: i32,
}

/// `wis_app[]` (constants.c). Indexed directly by wisdom 0..=25.
pub static WIS_APP: &[WisAppType] = &[
    WisAppType { bonus: 0 }, // wis = 0
    WisAppType { bonus: 0 }, // wis = 1
    WisAppType { bonus: 0 },
    WisAppType { bonus: 0 },
    WisAppType { bonus: 0 },
    WisAppType { bonus: 0 }, // wis = 5
    WisAppType { bonus: 0 },
    WisAppType { bonus: 0 },
    WisAppType { bonus: 0 },
    WisAppType { bonus: 0 },
    WisAppType { bonus: 0 }, // wis = 10
    WisAppType { bonus: 0 },
    WisAppType { bonus: 2 },
    WisAppType { bonus: 2 },
    WisAppType { bonus: 3 },
    WisAppType { bonus: 3 }, // wis = 15
    WisAppType { bonus: 3 },
    WisAppType { bonus: 4 },
    WisAppType { bonus: 5 }, // wis = 18
    WisAppType { bonus: 6 },
    WisAppType { bonus: 6 }, // wis = 20
    WisAppType { bonus: 6 },
    WisAppType { bonus: 6 },
    WisAppType { bonus: 7 },
    WisAppType { bonus: 7 },
    WisAppType { bonus: 7 }, // wis = 25
];

/// `lvl_maxdmg_weapon[LVL_IMMORT]` (config.c) — the heaviest weapon damage a
/// mortal of that level may wield. `handler.c` / `act.item.c` refuse to equip
/// anything above it and `stat` reports the value. Indexed by level
/// 0..=LVL_IMMORT-1 (101 entries).
pub static LVL_MAXDMG_WEAPON: &[i32] = &[
    15, 15, 15, 15, 15, 15, 15, 15, 15, 15, // 0 - 9
    20, 20, 20, 20, 20, 20, 20, 20, 20, 20, // 10 - 19
    25, 25, 25, 25, 25, 25, 25, 25, 25, 25, // 20 - 29
    30, 30, 30, 30, 30, 30, 30, 30, 30, 30, // 30 - 39
    35, 35, 35, 35, 35, 35, 35, 35, 35, 35, // 40 - 49
    45, 45, 45, 45, 45, 45, 45, 45, 45, 45, // 50 - 59
    50, 50, 50, 50, 50, 50, 50, 50, 50, 50, // 60 - 69
    60, 60, 60, 60, 60, 60, 60, 60, 60, 60, // 70 - 79
    75, 75, 75, 75, 75, 75, 75, 75, 75, 75, // 80 - 89
    100, 100, 100, 100, 100, 100, 100, 100, 100, 100, // 90 - 99
    100, // 100
];

/// `training_pts[LVL_IMMORT]` (config.c) — bonus training sessions awarded at
/// each level on `advance_level`. Indexed directly by level 0..=99.
pub static TRAINING_PTS: &[i32] = &[
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 0 - 9
    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 10 - 19
    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 20 - 29
    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 30 - 39
    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 40 - 49
    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 50 - 59
    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 60 - 69
    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 70 - 79
    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 80 - 89
    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 90 - 99
];

/// Clamp an ability score to a valid index into the 0..=25 `*_app` tables.
#[inline]
pub fn app_index(score: i32, len: usize) -> usize {
    score.clamp(0, (len - 1) as i32) as usize
}

/// `attack_hit_text[]` (fight.c) — weapon-attack verbs `(singular, plural)`
/// indexed by `attacktype - TYPE_HIT`. The index order is the contract: it
/// matches the `TYPE_HIT..TYPE_STAB` (#defines 1100..1114) sequence so the
/// weapon's `value[3]` (and a mob's BareHandAttack) selects the right verb.
pub static ATTACK_HIT_TEXT: &[(&str, &str)] = &[
    ("hit", "hits"),           // 0  TYPE_HIT
    ("sting", "stings"),       // 1  TYPE_STING
    ("whip", "whips"),         // 2  TYPE_WHIP
    ("slash", "slashes"),      // 3  TYPE_SLASH
    ("bite", "bites"),         // 4  TYPE_BITE
    ("bludgeon", "bludgeons"), // 5  TYPE_BLUDGEON
    ("crush", "crushes"),      // 6  TYPE_CRUSH
    ("pound", "pounds"),       // 7  TYPE_POUND
    ("claw", "claws"),         // 8  TYPE_CLAW
    ("maul", "mauls"),         // 9  TYPE_MAUL
    ("thrash", "thrashes"),    // 10 TYPE_THRASH
    ("pierce", "pierces"),     // 11 TYPE_PIERCE
    ("blast", "blasts"),       // 12 TYPE_BLAST
    ("punch", "punches"),      // 13 TYPE_PUNCH
    ("stab", "stabs"),         // 14 TYPE_STAB
];
