#!/usr/bin/env bash
# balance-check.sh — the Deltania Breathes balance gate.
#
# Runs balance_audit's real-lib gate: fails while the level curve has dead
# bands (5+ consecutive levels with no mobs, 10+ with no new gear unlock,
# 5+ with no quest target in range). GREEN is the W4 exit criterion; until
# then it prints the hole list.
#
# Usage: scripts/balance-check.sh [--audit]
#   --audit  also print the full per-level table (mobs/gear/quest per level)
set -euo pipefail
cd "$(dirname "$0")"

LIB="${MUD_LIB_PATH:-/web/deltamud/lib}"
if [ ! -d "$LIB" ]; then
    echo "balance-check: shipped lib not found at $LIB (set MUD_LIB_PATH)" >&2
    exit 2
fi

ARGS=(--release balance_audit_real_lib_gate -- --ignored)
if [ "${1:-}" = "--audit" ]; then
    ARGS+=(--nocapture)
fi

MUD_LIB_PATH="$LIB" cargo test "${ARGS[@]}"
