#!/bin/bash
# Build script for DeltaMUD Rust Edition

set -e

echo "Building DeltaMUD Rust Edition..."

# Build in release mode
cargo build --release

echo "Build complete!"
echo ""
echo "To run the MUD:"
echo "  Test mode:  MUD_PORT=4001 MUD_MOCK_DB=true ./target/release/deltamud"
echo "  MySQL mode: DATABASE_URL='mysql://user:pass@localhost/deltamud' ./target/release/deltamud"
echo ""
echo "Connect with: telnet localhost 4001 (or 4000 for default port)"