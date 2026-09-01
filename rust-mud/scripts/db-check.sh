#!/usr/bin/env bash
# db-check.sh — MySQL persistence gate (Deltania Breathes W1).
#
# Boots a THROWAWAY unprivileged mariadbd (own datadir and socket, with TCP
# disabled — never the production instance on 3306), loads the shipped
# deltamud_schema.sql, and runs
# the MUD_TEST_DATABASE_URL-gated integration tests in database.rs over that
# private Unix socket: the create/load/save round-trip, save idempotency, the
# mutation path, and the schema-vs-column-map parity test.
#
# All cleanup is by the PID we started — never pkill by name (a production
# mariadbd lives on this box!).
set -euo pipefail
CALLER_UID=$(id -u)
CALLER_GID=$(id -g)
if ! awk -v expected_uid="$CALLER_UID" -v expected_gid="$CALLER_GID" '
  /^Uid:/ {
    uid_seen = 1
    if ($2 != expected_uid || $3 != expected_uid || $4 != expected_uid || $5 != expected_uid) bad = 1
  }
  /^Gid:/ {
    gid_seen = 1
    if ($2 != expected_gid || $3 != expected_gid || $4 != expected_gid || $5 != expected_gid) bad = 1
  }
  /^Cap(Inh|Prm|Eff|Amb):/ { caps_seen++; if ($2 !~ /^0+$/) bad = 1 }
  END {
    safe = uid_seen && gid_seen && caps_seen == 4 && !bad
    safe = safe && expected_uid != 0 && expected_gid != 0
    exit !safe
  }
' "/proc/$$/status"; then
  echo "db-check: refusing root, set-ID, or capability-bearing execution; use an unprivileged development/CI user" >&2
  exit 77
fi

validate_root_directory_chain () {
  local path=$1
  while :; do
    [ -d "$path" ] && [ ! -L "$path" ] \
      && [ "$(stat -Lc %u -- "$path")" -eq 0 ] || return 1
    local mode
    mode=$(stat -Lc %a -- "$path") || return 1
    [ "$((8#$mode & 8#022))" -eq 0 ] || return 1
    [ "$path" = / ] && return 0
    path=${path%/*}
    [ -n "$path" ] || path=/
  done
}

MARIADBD_PATH=/usr/sbin/mariadbd
MARIADBD_BIN=$(readlink -e -- "$MARIADBD_PATH" 2>/dev/null || true)
[ -n "$MARIADBD_BIN" ] && [ -f "$MARIADBD_BIN" ] && [ ! -L "$MARIADBD_BIN" ] \
  && [ -x "$MARIADBD_BIN" ] \
  && [ "$(stat -Lc %u -- "$MARIADBD_BIN")" -eq 0 ] \
  && [ "$(stat -c %u -- "$MARIADBD_PATH")" -eq 0 ] \
  && validate_root_directory_chain "$(readlink -e -- "${MARIADBD_PATH%/*}")" \
  && validate_root_directory_chain "${MARIADBD_BIN%/*}" || {
  echo "db-check: MariaDB server path is missing or is not root-controlled" >&2
  exit 77
}
MARIADBD_MODE=$(stat -Lc %a -- "$MARIADBD_BIN")
[ "$((8#$MARIADBD_MODE & 8#6022))" -eq 0 ] || {
  echo "db-check: MariaDB server must not be set-ID or group/world writable" >&2
  exit 77
}
/usr/bin/python3 -I - "$MARIADBD_BIN" <<'PY' || {
import errno
import os
import sys

try:
    capability = os.getxattr(sys.argv[1], "security.capability")
except OSError as error:
    if error.errno not in (errno.ENODATA, errno.ENOTSUP):
        raise SystemExit(1)
else:
    if capability:
        raise SystemExit(1)
PY
  echo "db-check: MariaDB server must not carry file capabilities" >&2
  exit 77
}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)            # rust-mud/
REPO=$(cd "$ROOT/.." && pwd)            # deltamud/
WORK=$(mktemp -d /var/tmp/deltamud-db-check.XXXXXX)
DBDIR=$WORK/mariadb
SOCK=$WORK/mariadb.sock
MYSQL_PID=
MYSQL_TERM_SENT=0
MYSQL_KILL_SENT=0
: > "$WORK/mysql.log"

pid_has_exited () {
  local pid=$1
  local state
  if ! kill -0 "$pid" 2>/dev/null; then
    return 0
  fi
  state=$(ps -o stat= -p "$pid" 2>/dev/null || true)
  [[ "$state" == Z* ]]
}

wait_for_pid_exit () {
  local pid=$1
  local attempts=$2
  local attempt
  for ((attempt = 0; attempt < attempts; attempt++)); do
    pid_has_exited "$pid" && return 0
    sleep 0.1
  done
  pid_has_exited "$pid"
}

cleanup () {
  rc=$?
  trap - EXIT INT TERM
  cleanup_rc=0
  set +e
  if [ -n "$MYSQL_PID" ]; then
    if ! pid_has_exited "$MYSQL_PID"; then
      if kill -TERM "$MYSQL_PID" 2>/dev/null; then
        MYSQL_TERM_SENT=1
      elif ! pid_has_exited "$MYSQL_PID"; then
        cleanup_rc=71
      fi
    fi
    if ! wait_for_pid_exit "$MYSQL_PID" 50; then
      cleanup_rc=71
      if kill -KILL "$MYSQL_PID" 2>/dev/null; then
        MYSQL_KILL_SENT=1
      elif ! pid_has_exited "$MYSQL_PID"; then
        cleanup_rc=71
      fi
      if ! wait_for_pid_exit "$MYSQL_PID" 50; then
        echo "db-check: MariaDB PID $MYSQL_PID survived SIGKILL cleanup deadline" >&2
        cleanup_rc=71
      fi
    fi
    if pid_has_exited "$MYSQL_PID"; then
      wait "$MYSQL_PID" 2>/dev/null
      wait_rc=$?
      if [ "$MYSQL_TERM_SENT" -eq 0 ]; then
        echo "db-check: throwaway MariaDB exited before cleanup requested it" >&2
        cleanup_rc=71
      elif [ "$MYSQL_KILL_SENT" -eq 0 ] && [ "$wait_rc" -ne 0 ]; then
        echo "db-check: throwaway MariaDB returned $wait_rc after graceful TERM" >&2
        cleanup_rc=71
      fi
      MYSQL_PID=
    fi
  fi
  if [ "$rc" -eq 0 ] && [ "$cleanup_rc" -ne 0 ]; then
    rc=$cleanup_rc
  fi
  if [ "$rc" -eq 0 ]; then
    if ! rm -rf -- "$WORK"; then
      echo "db-check: could not remove throwaway data at $WORK" >&2
      rc=73
    fi
  else
    echo "db-check: failure artifacts preserved at $WORK" >&2
  fi
  [ "$rc" -ne 0 ] || echo "db-check: green"
  exit "$rc"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mariadb-install-db --no-defaults --datadir="$DBDIR" \
  --auth-root-authentication-method=normal --skip-test-db >/dev/null 2>&1
"$MARIADBD_BIN" --no-defaults --datadir="$DBDIR" --socket="$SOCK" \
  --sql-mode=NO_ENGINE_SUBSTITUTION --skip-ssl --skip-grant-tables --skip-networking \
  --pid-file="$WORK/mysqld.pid" >"$WORK/mysql.log" 2>&1 &
MYSQL_PID=$!

READY=0
for i in $(seq 1 100); do
  if mariadb --no-defaults --skip-ssl -u root --socket="$SOCK" -e 'SELECT 1' >/dev/null 2>&1; then
    READY=1
    break
  fi
  sleep 0.2
done
[ "$READY" -eq 1 ] || {
  echo "db-check: throwaway MariaDB did not become ready" >&2
  tail -40 "$WORK/mysql.log" >&2
  exit 70
}
if ! awk -v expected_uid="$CALLER_UID" -v expected_gid="$CALLER_GID" '
  /^Uid:/ {
    uid_seen = 1
    if ($2 != expected_uid || $3 != expected_uid || $4 != expected_uid || $5 != expected_uid) bad = 1
  }
  /^Gid:/ {
    gid_seen = 1
    if ($2 != expected_gid || $3 != expected_gid || $4 != expected_gid || $5 != expected_gid) bad = 1
  }
  /^Cap(Inh|Prm|Eff|Amb):/ { caps_seen++; if ($2 !~ /^0+$/) bad = 1 }
  END { exit !(uid_seen && gid_seen && caps_seen == 4 && !bad) }
' "/proc/$MYSQL_PID/status" 2>/dev/null; then
  echo "db-check: refusing a privileged or capability-bearing throwaway MariaDB process" >&2
  exit 77
fi
SKIP_NETWORKING=$(mariadb --no-defaults --skip-ssl -u root --socket="$SOCK" \
  --batch --skip-column-names -e 'SELECT @@GLOBAL.skip_networking')
[ "$SKIP_NETWORKING" = 1 ] || {
  echo "db-check: throwaway MariaDB unexpectedly has TCP networking enabled" >&2
  exit 77
}
mariadb --no-defaults --skip-ssl -u root --socket="$SOCK" -e 'CREATE DATABASE IF NOT EXISTS deltamud'
[ -f "$REPO/deltamud_schema.sql" ] || { echo "db-check: $REPO/deltamud_schema.sql missing" >&2; exit 2; }
mariadb --no-defaults --binary-mode --skip-ssl -u root --socket="$SOCK" deltamud \
  < "$REPO/deltamud_schema.sql"

cd "$ROOT"
SOCKET_URL=${SOCK//\//%2F}
EXPECTED_MYSQL_TESTS=12
MYSQL_TEST_LIST=$(cargo test --locked mysql_integration -- --list)
MYSQL_TEST_COUNT=$(printf '%s\n' "$MYSQL_TEST_LIST" | awk '/mysql_integration.*: test$/ { count++ } END { print count + 0 }')
[ "$MYSQL_TEST_COUNT" -eq "$EXPECTED_MYSQL_TESTS" ] || {
  echo "db-check: expected $EXPECTED_MYSQL_TESTS mysql integration tests, found $MYSQL_TEST_COUNT" >&2
  exit 74
}
MUD_TEST_DATABASE_URL="mysql://root@localhost/deltamud?socket=$SOCKET_URL" \
  cargo test --locked mysql_integration -- --nocapture --test-threads 1
if pid_has_exited "$MYSQL_PID"; then
  echo "db-check: throwaway MariaDB exited before the persistence gate completed" >&2
  exit 71
fi
echo "db-check: tests passed; validating graceful daemon cleanup"
