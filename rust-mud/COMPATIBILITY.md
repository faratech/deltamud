# DeltaMUD C/Rust Compatibility Guide

## Database Compatibility

### ⚠️ WARNING: Databases are NOT directly compatible!

The Rust version implements a simplified schema that is missing many fields from the original C version.

#### Missing Features in Base Rust Version:
- Bank accounts (gold, amethyst, bronze, silver, copper, steel)
- Arena statistics
- Clan system
- Quest system
- Player preferences (page_length, etc)
- Immortal/admin levels
- Combat modifiers (power, defense, technique)
- Conditions (hunger, thirst, drunk)
- Death tracking statistics
- Alignment system
- Language system
- Deity system
- And 40+ other fields

#### Password Incompatibility:
- C version uses old Unix crypt() with `pwd_new` flag
- Rust version uses SHA-256 hashing
- Existing passwords WILL NOT WORK

### Using the Compatibility Layer

To use existing DeltaMUD databases, use the `database_compat.rs` module:

```rust
// In main.rs, replace:
use database::Database;
// With:
use database_compat::CompatDatabase;

// Use CompatDatabase instead of Database
let db = CompatDatabase::new(&db_url)?;
```

This compatibility layer:
- Reads all 83 columns from the original schema
- Maps available data to Rust structures
- Preserves unused fields on save
- Handles password compatibility warnings

### Migration Path

1. **For New MUDs**: Use the standard `database.rs` with simplified schema
2. **For Existing MUDs**: 
   - Use `database_compat.rs` to preserve all player data
   - Implement missing features as needed
   - Consider password reset for all players

## World File Compatibility

### ✅ World files ARE mostly compatible!

The Rust file loader can read original CircleMUD format files:

#### Working Features:
- ✅ Zones (.zon files)
- ✅ Rooms (.wld files) - vnums, names, descriptions, exits
- ✅ Mobiles (.mob files) - basic stats and descriptions
- ✅ Objects (.obj files) - basic properties
- ✅ Shops (.shp files) - basic functionality

#### Not Yet Implemented:
- ❌ DG Scripts triggers (T command in files)
- ❌ Extra descriptions (E command)
- ❌ Some advanced flags
- ❌ Zone reset commands (full implementation)

### Using World Files

Simply point to your existing lib directory:

```bash
export MUD_LIB_PATH="/web/deltamud/lib"
cargo run
```

The file loader will:
1. Read index files from each subdirectory
2. Parse all zone/room/mob/obj files
3. Skip over unrecognized commands with warnings
4. Load what it can understand

### File Format Example

The Rust loader understands standard CircleMUD format:

```
#3001
The Temple of Midgaard~
You are in the southern end of the temple.
~
30 8 0
D0
~
~
0 -1 3005
S
```

## Recommended Approach

### For Production Use with Existing Data:

1. **Use the compatibility layer** (`database_compat.rs`)
2. **Reset all passwords** - old crypt() won't work
3. **Test thoroughly** - some features are stubs
4. **Implement missing features** as needed

### For New Development:

1. **Use the standard modules** - cleaner, simpler
2. **Import world files** - they work fine
3. **Start fresh with players** - avoid schema issues

## Quick Compatibility Check

Run this SQL to check your database:

```sql
-- Check schema version
SELECT COUNT(*) as column_count 
FROM information_schema.columns 
WHERE table_schema = 'deltamud' 
AND table_name = 'player_main';

-- If result is ~83, you have the full C schema
-- If result is ~30, you have the Rust schema
```

## Data Safety

**ALWAYS BACKUP** before attempting any migration:

```bash
mysqldump -u root -p deltamud > deltamud_backup.sql
```

The Rust version will not corrupt world files (read-only), but database operations could lose data if using the wrong compatibility mode.