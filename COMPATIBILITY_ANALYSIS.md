# Database Compatibility Analysis: C vs Rust Implementation

## Overview
This document identifies the compatibility issues between the original C DeltaMUD database implementation (`src/dbinterface.c`) and the Rust reimplementation (`rust-mud/src/database.rs`).

## Critical Schema Differences

### 1. Missing Columns in Rust Implementation
The original C implementation uses 83 columns in `player_main` table, while the Rust implementation only has about 30 columns. Missing critical columns include:

#### Player Stats & Specials
- `power`, `mpower`, `defense`, `mdefense`, `technique` - Combat modifiers
- `deity` - Player's chosen deity
- `bank_gold` - Bank account gold storage
- `str_add` - Additional strength modifier (0-100 for 18 strength)

#### Player Preferences & Settings
- `talks1`, `talks2`, `talks3` - Language/talk settings
- `wimp_level` - Auto-flee HP threshold
- `freeze_level` - Immortal freeze status
- `invis_level` - Invisibility level
- `load_room` - Room to load player into
- `pref`, `pref2` - Player preference flags
- `bad_pws` - Bad password attempt counter
- `cond1`, `cond2`, `cond3` - Hunger/thirst/drunk conditions

#### Gameplay Features
- `death_timer` - Death/respawn timer
- `citizen` - Citizenship status
- `training` - Training points/status
- `newbie` - New player flag
- `arena` - Arena status/statistics
- `spells_to_learn` - Available spell learning points
- `questpoints`, `nextquest`, `countdown`, `questobj`, `questmob` - Quest system
- `recall_level`, `retreat_level` - Spell/skill levels
- `trust` - Trust level (different from immortal level)
- `bail_amt` - Jail bail amount
- `wins`, `losses` - PvP statistics

#### Immortal/Builder Features
- `godcmds1`, `godcmds2`, `godcmds3`, `godcmds4` - God command permissions
- `mapx`, `mapy` - Map coordinates
- `buildmodezone`, `buildmoderoom` - Builder mode settings
- `tloadroom` - Temporary load room

### 2. Column Name Differences
Several columns have different names between implementations:
- C: `intel` → Rust: `int_base` (intelligence stat)
- C: `move` → Rust: `move_points`
- C: `pwd` → Rust: `password`
- C: `host` → Rust: `host` (but not used in Rust load/save)

### 3. Data Type Differences
- **Timestamps**: C uses `BIGINT` for birth/played/last_logon, Rust uses `TIMESTAMP`
- **Password**: C stores as `VARCHAR(50)`, Rust as `VARCHAR(64)` for SHA-256
- **Name Length**: C allows 30 chars, Rust only 20 chars

### 4. Missing Tables/Features
The Rust implementation adds a `player_objects` table that doesn't exist in the C version, while the C version handles object saving through file-based storage.

## Functional Differences

### 1. Password Handling
- **C**: Supports both old crypt() passwords and can upgrade to SHA-256
- **Rust**: Only supports SHA-256, returns false for old passwords

### 2. Column Mapping System
- **C**: Uses a sophisticated macro system (`COLUMN_NRM`, `COLUMN_STR`) with runtime column mapping
- **Rust**: Uses hardcoded column positions in SQL queries

### 3. Data Validation
- **C**: Has extensive validation and clamping for corrupted values (gold, bank_gold, etc.)
- **Rust**: No validation on load/save

### 4. Affect/Skill Storage
- **C**: Batch inserts for affects and skills
- **Rust**: Individual inserts for each affect

### 5. Time Tracking
- **C**: Updates `played` time on save, handles logon time tracking
- **Rust**: Doesn't update played time

## Migration Requirements

To make the Rust implementation compatible with existing DeltaMUD data:

### 1. Schema Updates
Add all missing columns to the Rust table creation with appropriate defaults:
```sql
ALTER TABLE player_main ADD COLUMN bank_gold INT DEFAULT 0;
ALTER TABLE player_main ADD COLUMN deity TINYINT DEFAULT 0;
ALTER TABLE player_main ADD COLUMN power INT DEFAULT 0;
-- ... (all other missing columns)
```

### 2. Code Updates Required

#### A. Update Character Structure
Add missing fields to `PlayerData` and `CharPoints` structures in `character.rs`

#### B. Update Database Mapping
- Modify `row_to_character()` to read all columns
- Update `save_player()` to write all columns
- Add validation/clamping logic for corrupted values

#### C. Password Compatibility
Implement crypt() password verification fallback or migration system

#### D. Time Tracking
Implement proper played time calculation on save

### 3. Data Migration Script
Create a migration script to:
- Convert BIGINT timestamps to TIMESTAMP columns
- Migrate existing passwords to SHA-256 if needed
- Set appropriate defaults for new columns

## Risk Assessment

### High Risk Issues
1. **Data Loss**: Missing columns mean player data won't be preserved
2. **Password Incompatibility**: Players with old passwords can't login
3. **Corruption**: No validation means corrupted values persist

### Medium Risk Issues
1. **Feature Loss**: Many gameplay features won't work (quests, arena, etc.)
2. **Time Tracking**: Player time statistics will be incorrect

### Low Risk Issues
1. **Performance**: Individual inserts vs batch inserts for affects
2. **Name Length**: Shorter name limit might truncate existing names

## Recommendations

1. **Immediate**: Update Rust schema to match C implementation exactly
2. **Short-term**: Implement all missing column mappings
3. **Medium-term**: Add data validation and corruption handling
4. **Long-term**: Consider modernizing schema while maintaining compatibility