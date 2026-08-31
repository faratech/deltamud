#!/usr/bin/env bash
# db-check.sh — MySQL persistence gate (Deltania Breathes W1).
#
# Boots a THROWAWAY mariadbd (own datadir, socket, and port 3307 — never the
# production instance on 3306), loads the shipped deltamud_schema.sql, and runs
# the MUD_TEST_DATABASE_URL-gated integration tests in database.rs: the
# create/load/save round-trip, save idempotency, the mutation path, and the
# schema-vs-column-map parity test.
#
# All cleanup is by the PID we started — never pkill by name (a production
# mariadbd lives on this box!).
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)            # rust-mud/
REPO=$(cd "$ROOT/.." && pwd)            # deltamud/
WORK=/tmp/db-check-run
DBDIR=/tmp/db-check-mariadb
SOCK=/tmp/db-check.sock
PORT=3307

mkdir -p "$WORK"
rm -rf "$DBDIR" "$SOCK"
: > "$WORK/mysql.log"

mariadb-install-db --datadir="$DBDIR" --auth-root-authentication-method=normal --skip-test-db >/dev/null 2>&1
/usr/sbin/mariadbd --user=root --datadir="$DBDIR" --socket="$SOCK" \
  --port=$PORT --bind-address=127.0.0.1 --sql-mode=NO_ENGINE_SUBSTITUTION --skip-ssl \
  --skip-grant-tables --skip-networking=0 --pid-file="$WORK/mysqld.pid" >"$WORK/mysql.log" 2>&1 &
MYSQL_PID=$!
trap '[ -n "${MYSQL_PID:-}" ] && kill "$MYSQL_PID" 2>/dev/null || true' EXIT

for i in $(seq 1 100); do
  mariadb --no-defaults --skip-ssl -u root --socket="$SOCK" -e 'SELECT 1' >/dev/null 2>&1 && break
  sleep 0.2
done
mariadb --no-defaults --skip-ssl -u root --socket="$SOCK" -e 'CREATE DATABASE IF NOT EXISTS deltamud'
[ -f "$REPO/deltamud_schema.sql" ] || { echo "db-check: $REPO/deltamud_schema.sql missing" >&2; exit 2; }
mariadb --no-defaults --skip-ssl -u root --socket="$SOCK" deltamud < "$REPO/deltamud_schema.sql"

cd "$ROOT"
MUD_TEST_DATABASE_URL="mysql://root@127.0.0.1:$PORT/deltamud" \
  cargo test mysql_integration -- --nocapture --test-threads 1
echo "db-check: green"
