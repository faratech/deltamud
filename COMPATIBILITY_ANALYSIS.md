# DeltaMUD Rust Implementation - Feature Parity Analysis

## Overview

This document provides a comprehensive analysis of features that need to be implemented in the Rust version of DeltaMUD to achieve 1:1 feature parity with the C implementation.

## Current Implementation Status

### ✅ Implemented Features (Basic Versions)
- TCP networking with async I/O
- Basic character creation and management
- Room navigation and exits
- Basic combat system (melee only)
- Simple magic system (limited spells)
- MySQL database integration
- World file loading (partial)
- Basic commands (~30 commands)
- HP/Mana/Move regeneration
- Object/inventory system (basic)
- Equipment wearing/removing
- Simple affects system

### ❌ Missing Critical Features

## 1. Special Systems

### Clan System (`clan.c`, `clan.h`)
**Priority: HIGH**
- Data structures for clans
- Clan membership tracking
- Clan chat channels
- Clan ranks and leadership
- Commands: clan, csay, ctell

### Arena System (`arena.c`)
**Priority: MEDIUM**
- Arena room flagging
- PvP challenge system
- Arena-specific combat rules
- Arena statistics tracking
- Commands: challenge, accept, decline, arena

### Auction System (`auction.c`, `auction.h`)
**Priority: MEDIUM**
- Real-time auction mechanics
- Bid tracking
- Item transfer on sale
- Commands: auction, bid, auctioneer

### Board System (`boards.c`, `boards.h`)
**Priority: HIGH**
- Message boards with persistence
- Board permissions
- Message reading/writing
- Multiple board support
- Commands: read, write, remove, boards

### Mail System (`mail.c`, `mail.h`)
**Priority: MEDIUM**
- In-game mail delivery
- Postmaster NPCs
- Mail storage and retrieval
- Commands: mail, check, receive

### Deity System (`deity.c`)
**Priority: LOW**
- 15 deity definitions
- Deity selection
- Deity-based affects
- Prayer/worship mechanics

### Language System (`language.c`)
**Priority: LOW**
- Multiple language support
- Language comprehension
- Language learning
- Communication filtering

## 2. OLC (Online Creation) System

**Priority: CRITICAL**

### Room Editor (`redit.c`)
- Create/edit rooms
- Set room flags
- Edit descriptions
- Manage exits

### Object Editor (`oedit.c`)
- Create/edit objects
- Set object values
- Manage affects
- Set wear flags

### Mobile Editor (`medit.c`)
- Create/edit NPCs
- Set stats and flags
- Assign special procedures
- Set movement patterns

### Zone Editor (`zedit.c`)
- Zone reset commands
- Zone timing
- Zone flags
- Reset sequences

### Shop Editor (`sedit.c`)
- Shop creation
- Buy/sell lists
- Shop keeper assignment
- Price management

### Action Editor (`aedit.c`)
- Social action editing
- Command creation

### Help Editor (`hedit.c`)
- Help file management
- Dynamic help system

## 3. DG Scripts System

**Priority: CRITICAL**

### Core Components Needed:
- Script parser (`dg_scripts.c`)
- Trigger system (`dg_triggers.c`)
- Event queue (`dg_event.c`)
- Script variables
- Script commands for:
  - Mobiles (`dg_mobcmd.c`)
  - Objects (`dg_objcmd.c`)
  - Rooms (`dg_wldcmd.c`)
- Script OLC (`dg_olc.c`)
- Script communication (`dg_comm.c`)

### Trigger Types to Implement:
- Mobile: greet, death, command, speech, act, fight, hitprcnt, bribe
- Object: command, timer, get, drop, give, wear, remove, load
- Room: enter, exit, command, speech, drop, cast, leave, door

## 4. Advanced Combat System

**Priority: HIGH**

### Missing Combat Features:
- Special attacks:
  - Backstab (thief skill)
  - Bash (warrior skill)
  - Kick (basic combat)
  - Trip
  - Disarm
  - Berserk
- Targeting system
- Riposte mechanics
- Combat techniques
- Power/Defense modifiers
- Hit/Dam rolls
- AC (Armor Class) calculations
- THAC0 tables per class

## 5. Character Conditions & Stats

**Priority: HIGH**

### Missing Conditions:
- Hunger (0-24 scale)
- Thirst (0-24 scale)
- Drunk (0-24 scale)
- Effects on regeneration
- Death from hunger/thirst

### Missing Stats:
- Alignment (-1000 to 1000)
- Experience to level
- Hit/Dam bonuses
- AC adjustments
- Saving throws (5 types)
- Skill percentages
- Spell memorization

## 6. Economy System

**Priority: HIGH**

### Banking:
- Bank accounts
- Gold storage
- Withdraw/deposit
- Balance checking
- Bank rooms/ATMs

### Currency:
- Multiple currency types (gold, silver, copper, etc.)
- Currency conversion
- Shop pricing

## 7. Player Housing

**Priority: MEDIUM**

### House System (`house.c`):
- House ownership
- House purchasing
- Access control (guests)
- Storage in houses
- House taxes/upkeep

## 8. Quest System

**Priority: MEDIUM**

### Automated Quests (`quest.c`):
- Quest generation
- Quest tracking
- Quest rewards
- Quest NPCs
- Autoquest command

## 9. Immortal/Admin Features

**Priority: CRITICAL**

### Level System:
- LVL_IMPL (105)
- LVL_GRGOD (104)  
- LVL_GOD (103)
- LVL_IMMORT (101)

### Missing Wizard Commands:
- advance - Set player level
- at - Execute at location
- ban/unban - Site banning
- copyover - Hot reboot
- dc - Disconnect player
- force - Force action
- freeze/thaw - Freeze player
- gecho - Global echo
- goto - Teleport
- load - Load mob/obj
- peace - Stop combat
- purge - Remove objects
- reload - Reload files
- restore - Restore player
- send - Send text
- set - Set values
- show - Show game info
- shutdown - Stop server
- snoop - Watch player
- stat - Show stats
- switch - Control mob
- syslog - System log
- vnum - Find vnums
- vstat - Virtual stats
- wizhelp - Wizard help
- wizlock - Lock game
- wiznet - Immortal chat
- zreset - Reset zones

## 10. Additional Systems

### Communication:
- Gossip channel
- Auction channel  
- Arena channel
- Clan channels
- Tell history
- AFK system
- Ignore lists

### Player Preferences:
- Display toggles (brief, compact, etc.)
- Autoloot, autogold, autosplit
- Prompt customization
- Color preferences
- Page length
- Screen width

### Utility Commands:
- alias - Command aliases
- bug/idea/typo - Reporting
- time - Game time
- weather - Weather info
- who - Enhanced who list
- whois - Player info
- score - Full score
- affects - Show affects
- practice - Skill practice
- train - Stat training

### Special Features:
- Mounts (buck system)
- Camping
- Carving/crafting
- Brewing potions
- Snow/weather effects
- Day/night cycle
- Mob memory
- Mob aggression
- Mob assists

## Database Schema Differences

### Missing Columns in player_main (53 columns):
- `power`, `mpower`, `defense`, `mdefense`, `technique` - Combat modifiers
- `deity` - Player's chosen deity
- `bank_gold`, `bank_amethyst`, `bank_bronze`, `bank_silver`, `bank_copper`, `bank_steel` - Bank accounts
- `str_add` - Additional strength modifier
- `talks1`, `talks2`, `talks3` - Language/talk settings
- `wimp_level` - Auto-flee HP threshold
- `freeze_level` - Immortal freeze status
- `invis_level` - Invisibility level
- `load_room` - Room to load player into
- `pref`, `pref2` - Player preference flags
- `bad_pws` - Bad password attempt counter
- `cond1`, `cond2`, `cond3` - Hunger/thirst/drunk conditions
- `death_timer` - Death/respawn timer
- `citizen` - Citizenship status
- `training` - Training points/status
- `newbie` - New player flag
- `arena` - Arena status/statistics
- `spells_to_learn` - Available spell learning points
- `questpoints`, `nextquest`, `countdown`, `questobj`, `questmob` - Quest system
- `recall_level`, `retreat_level` - Spell/skill levels
- `trust` - Trust level
- `bail_amt` - Jail bail amount
- `wins`, `losses` - PvP statistics
- `godcmds1`, `godcmds2`, `godcmds3`, `godcmds4` - God command permissions
- `mapx`, `mapy` - Map coordinates
- `buildmodezone`, `buildmoderoom` - Builder mode settings
- `tloadroom` - Temporary load room
- `page_length` - Display preferences
- `screen_width` - Display preferences
- `poofin`, `poofout` - Immortal poof messages
- `prompt` - Custom prompt
- `color_flag1`, `color_flag2` - Color preferences
- `afk_msg` - AFK message
- And more...

### Missing Tables:
- `clan_main` - Clan definitions
- `clan_members` - Membership tracking
- `boards` - Board definitions  
- `board_messages` - Posted messages
- `houses` - Player housing
- `mail` - Mail messages
- `quest_tracking` - Active quests
- `auction_items` - Current auctions
- `player_aliases` - Command aliases
- `ban_sites` - Site banning
- `help_entries` - Dynamic help

## Implementation Priority Order

### Phase 1: Critical Infrastructure
1. **Database Compatibility** - Add all missing columns/tables
2. **Immortal/admin system** - Needed for testing and management
3. **OLC system** - Needed for world building
4. **DG Scripts** - Core game mechanics depend on this
5. **Full combat system** - Complete the combat mechanics

### Phase 2: Core Features  
1. **Board system** - Player communication
2. **Clan system** - Player organizations
3. **Banking/economy** - Game economy
4. **Enhanced communication** - Channels and tells
5. **Player preferences** - Quality of life

### Phase 3: Advanced Features
1. **Quest system** - Automated content
2. **Arena system** - PvP content
3. **Auction system** - Player economy
4. **Mail system** - Async communication
5. **House system** - Player ownership

### Phase 4: Polish
1. **Deity system** - Role-play element
2. **Language system** - Role-play element
3. **Advanced crafting** - Additional content
4. **Weather effects** - Immersion
5. **All remaining commands** - Completeness

## Estimated Effort

Based on the C implementation being ~71,000 lines and Rust having ~15,000 lines:

- **Total new code needed**: ~40,000-50,000 lines
- **Database changes**: 53 columns + 10 tables
- **Commands to implement**: ~150 commands
- **Systems to build**: 15+ major systems
- **Estimated time**: 6-12 months for single developer

## File Format Support

### Currently Missing:
- E (extra descriptions) in world files
- T (trigger assignments) in world files
- Complex zone reset commands
- Shop attitudes/messages
- Advanced mob flags
- Spell/skill assignments

## Recommendations

1. **Start with database compatibility** - Without this, no progress is possible
2. **Use the database_compat.rs** approach for existing data
3. **Implement OLC early** to enable content creation
4. **Build DG Scripts** before advanced features
5. **Test each phase** thoroughly before moving on
6. **Keep C version running** during transition

## Conclusion

The Rust implementation currently has only ~20-25% of DeltaMUD's features. Full parity requires:

- Adding 53 database columns
- Creating 10+ new tables  
- Implementing 15+ major subsystems
- Adding ~150 commands
- Building the entire OLC system
- Creating the DG Scripts engine
- Implementing all immortal features

This is a substantial project that will require careful planning and systematic implementation to achieve true 1:1 feature parity.