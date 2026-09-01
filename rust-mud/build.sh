#!/usr/bin/env bash
# Build script for DeltaMUD Rust Edition

set -Eeuo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)

echo "Building DeltaMUD Rust Edition..."

# Build in release mode
cargo build --manifest-path "$SCRIPT_DIR/Cargo.toml" --release --locked

echo "Build complete!"
echo ""
echo "To run the MUD:"
echo "  Test mode:  MUD_MOCK_DB=true MUD_BIND=127.0.0.1 MUD_PORT=4001 ./target/release/deltamud"
echo "  MySQL mode: MUD_MOCK_DB=false MUD_BIND=127.0.0.1 DATABASE_URL='mysql://user:pass@localhost/deltamud' ./target/release/deltamud"
echo ""
echo "Connect with: telnet localhost 4001 (or 4000 for default port)"
