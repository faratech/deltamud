# DeltaMUD

DeltaMUD is a text-based multiplayer RPG (MUD) virtual world engine derived from CircleMUD 3.0, featuring a modern **Rust** reimplementation alongside the original C reference.

## Project Structure

- **`rust-mud/`**: Modern Rust engine (~75k lines, ~83 modules). High performance, memory-safe, single-owner architecture bootable against original world files with verified behavioral parity.
- **`src/`**: Legacy C source codebase (historical reference).
- **`lib/`**: World files, zones, rooms, mobiles, objects, and text configurations.
- **`deltamud_schema.sql`**: MySQL / MariaDB database schema for persistence.

## Quickstart (Rust Engine)

### Prerequisites
- Rust 1.75+ (or latest stable)
- MySQL / MariaDB (optional, for persistent player data)

### Build & Run
```bash
cd rust-mud

# Build the server
cargo build --release

# Run test suite (799+ passing tests)
cargo test

# Launch local server
cargo run --release
```

For detailed engine architecture and compatibility audit details, see `rust-mud/README.md` and `COMPATIBILITY_ANALYSIS.md`.

## License
See `AGREEMENT` for historical CircleMUD licensing terms and derivative agreements.