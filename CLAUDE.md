# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build Commands

```bash
# Full build from source directory
cd src && make clean && make all

# Build just the main server
cd src && make circle

# Build utilities
cd src/util && make all
```

## Running the MUD

```bash
# Production mode with auto-restart
./autorun

# Debug/test mode (direct execution)
bin/circle -q 4000

# Control files for autorun script:
touch .fastboot    # Quick reboot (5 seconds)
touch .killscript  # Stop autorun permanently
touch pause        # Pause rebooting temporarily
```

## Architecture Overview

DeltaMUD is a CircleMUD 3.0 derivative with MySQL integration. The codebase follows a traditional MUD architecture:

### Core Systems
- **Main Loop** (`src/comm.c`): Central game loop, socket handling, player I/O
- **Database** (`src/dbinterface.c`): MySQL integration for persistent player storage
  - Tables: `player_main`, `player_affects`, `player_skills`
- **Command Interpreter** (`src/interpreter.c`): Parses and routes player commands
- **World Database** (`src/db.c`): Loads and manages game world data

### Game Mechanics
- **Combat** (`src/fight.c`): Combat system, damage calculation
- **Magic** (`src/magic.c`, `src/spells.c`): Spell system implementation
- **Skills** (`src/spell_parser.c`): Skill/spell learning and usage
- **Movement** (`src/act.movement.c`): Room navigation and exits

### Content Creation (OLC)
- `src/redit.c` - Room editor
- `src/oedit.c` - Object editor
- `src/medit.c` - Mobile (NPC) editor
- `src/zedit.c` - Zone editor
- `src/sedit.c` - Shop editor

### Special Features
- **DG Scripts** (`src/dg_*.c`): Trigger-based scripting system
- **Clans** (`src/clan.c`): Player organization system
- **Arena** (`src/arena.c`): PvP combat zone
- **Auction** (`src/auction.c`): Item trading system

## Key Configuration

- Default port: 4000
- Data directory: `lib/`
- Player files: `lib/etc/players`, `lib/plrobjs/`
- World files: `lib/world/` (zones, rooms, mobs, objects, shops, triggers)
- MySQL database: `deltamud` (see `deltamud_schema.sql`)

## Common Development Tasks

```bash
# Check syntax without running
bin/circle -c

# Run in mini-mud mode (limited zones)
bin/circle -m 4000

# Update wizard list
bin/autowiz

# Reset a player password
bin/mudpasswd <player_name>
```

## Testing Changes

1. For code changes: Recompile and restart the MUD
2. For world file changes: Use OLC or edit files directly, then reboot
3. For database changes: Apply to MySQL, may require code updates

Note: The MUD uses copyover functionality for seamless reboots while preserving player connections.