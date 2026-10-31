# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

> **Status (verified 2026-10-31).**
> - **Two engines.** The Rust engine in `rust-mud/` is the actively developed, supported server.
>   It has its own `rust-mud/CLAUDE.md`, `rust-mud/README.md` and `rust-mud/docs/RUNBOOK.md`.
>   This file documents the **legacy C codebase** (`src/`, `bin/circle`), which is kept as the
>   behavior oracle for parity work. The repo-level `README.md` explains the split.
> - **Not running on windowsforum.com's server.** `deltamud.service` (`bin/circle -q 4000`) and
>   `deltamud-websocat.service` have been **masked** (symlinked to `/dev/null`) since 2026-09-28. The
>   original unit files sit beside them as `*.pre-mask-20260928`. "Production mode" below describes
>   the historical `autorun` setup, not anything live.
> - Git submodule of `/web` (origin `faratech/deltamud`, branch `main`). Fetch and check
>   ahead/behind before work, commit and push here, then bump the pointer in `/web` with the
>   pathspec form (`git commit -m … -- deltamud`).
> - The 2026-10-09 security fixes (issues #421–#433: connection slots, login throttling, clan
>   ranks, GMCP visibility) and the 2026-10-13 gameplay/persistence fixes landed after this file's
>   last full revision. Read `git log` and the Rust runbook before relying on behaviour notes below.

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