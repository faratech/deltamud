#!/usr/bin/env bash
# Parity battery: proves rust-mud against the C oracle side by side.
#
# Isolation: everything runs inside a private network namespace (`unshare -n`)
# with its own loopback and a throwaway MariaDB on 127.0.0.1:3306 (the C binary
# hardcodes that endpoint), so neither server can touch production data. The
# world/lib is a fresh copy in /tmp/parity-lib every run. All cleanup is done
# by PID - never pkill by name (a host mariadbd lives here!).
#
# Usage:
#   scripts/parity-check.sh            # boot both servers, run the battery, diff
#   PROBE=1 scripts/parity-check.sh    # boot + drive, keep raw transcripts
#
# Inputs:
#   scripts/parity/scenario.txt        # prompt->answer login map + command list
#   scripts/parity/driver.py           # expect-style driver
#
# Outputs in a fresh /tmp/deltamud-parity.XXXXXX directory printed at exit:
#   raw_c.txt / raw_r.txt              # raw transcripts
#   norm_c.txt / norm_r.txt            # normalized (ANSI stripped, digits->N)
#   diff.txt                           # unified diff (empty == converged)
set -Eeuo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
MUD_DIR=$(cd "$HERE/.." && pwd)
REPO_DIR=$(cd "$MUD_DIR/.." && pwd)
if [ -z "${RUST_BIN+x}" ]; then
  echo "[parity] building current Rust source..."
  cargo build --release --manifest-path "$MUD_DIR/Cargo.toml"
  RUST_BIN=$MUD_DIR/target/release/deltamud
fi
C_BIN=${C_BIN:-$REPO_DIR/bin/circle}
SEED=${MUD_RNG_SEED:-12345}
PARITY_TIMEOUT_SECONDS=${PARITY_TIMEOUT_SECONDS:-240}

if [ ! -x "$RUST_BIN" ] || [ ! -x "$C_BIN" ]; then
  echo "[parity] required executable missing (Rust: $RUST_BIN; C: $C_BIN)" >&2
  exit 65
fi
if ! [[ "$PARITY_TIMEOUT_SECONDS" =~ ^[1-9][0-9]*$ ]]; then
  echo "[parity] PARITY_TIMEOUT_SECONDS must be a positive integer" >&2
  exit 64
fi
RUST_BIN=$(readlink -f -- "$RUST_BIN")
C_BIN=$(readlink -f -- "$C_BIN")

WORK=${PARITY_WORK:-$(mktemp -d /tmp/deltamud-parity.XXXXXX)}
mkdir -p "$WORK" "$HERE/parity"
export PARITY_WORK="$WORK"
export PARITY_RUST_BIN="$RUST_BIN"
export PARITY_C_BIN="$C_BIN"
export PARITY_MUD_DIR="$MUD_DIR"
export PARITY_REPO_DIR="$REPO_DIR"

# The inner script uses a QUOTED heredoc marker ('INNER') so nothing is
# expanded by this outer shell: every $var is evaluated by the inner shell.
cat > "$WORK/netns.sh" <<'INNER'
set -Eeuo pipefail
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
WORK=${PARITY_WORK:?missing parity work directory}
SEED=${MUD_RNG_SEED:-12345}
RUST_BIN=${PARITY_RUST_BIN:?missing Rust binary}
C_BIN=${PARITY_C_BIN:?missing C binary}
MUD_DIR=${PARITY_MUD_DIR:?missing Rust MUD directory}
REPO_DIR=${PARITY_REPO_DIR:?missing repository directory}
LIB_C=$WORK/lib-c
LIB_R=$WORK/lib-r
DBDIR=$WORK/mariadb
SOCK=$WORK/mariadb.sock
MYSQL_PID=
C_PID=
R_PID=
pid_has_exited () {
  local pid=$1
  local state
  if ! kill -0 "$pid" 2>/dev/null; then
    return 0
  fi
  state=$(ps -o stat= -p "$pid" 2>/dev/null || true)
  [[ "$state" == Z* ]]
}
stop_pid () {
  local pid=$1
  local label=${2:-process}
  local was_running=1
  local signal_sent=0
  local wait_rc=0
  [ -n "$pid" ] || return 0
  if pid_has_exited "$pid"; then
    was_running=0
  else
    if kill -TERM "$pid" 2>/dev/null; then
      signal_sent=15
    elif ! pid_has_exited "$pid"; then
      echo "[parity] could not terminate PID $pid" >&2
      return 71
    fi
    for _ in $(seq 1 50); do
      pid_has_exited "$pid" && break
      sleep 0.1
    done
    if ! pid_has_exited "$pid"; then
      if kill -KILL "$pid" 2>/dev/null; then
        signal_sent=9
      elif ! pid_has_exited "$pid"; then
        echo "[parity] could not kill PID $pid" >&2
        return 71
      fi
      for _ in $(seq 1 50); do
        pid_has_exited "$pid" && break
        sleep 0.1
      done
      if ! pid_has_exited "$pid"; then
        echo "[parity] PID $pid survived cleanup deadline" >&2
        return 71
      fi
    fi
  fi
  wait "$pid" 2>/dev/null || wait_rc=$?
  if [ "$was_running" -eq 0 ]; then
    echo "[parity] $label PID $pid exited before its acknowledged stop (status=$wait_rc)" >&2
    if [ "$wait_rc" -eq 0 ]; then
      return 74
    fi
    return "$wait_rc"
  fi
  case "$signal_sent:$wait_rc" in
    15:0|15:143|9:137) return 0 ;;
    *)
      echo "[parity] $label PID $pid returned unexpected status $wait_rc after stop signal $signal_sent" >&2
      if [ "$wait_rc" -eq 0 ]; then
        return 74
      fi
      return "$wait_rc"
      ;;
  esac
}
cleanup () {
  rc=$?
  cleanup_rc=0
  trap - EXIT INT TERM
  set +e
  stop_pid "$C_PID" "C oracle" || cleanup_rc=71
  stop_pid "$R_PID" "Rust server" || cleanup_rc=71
  stop_pid "$MYSQL_PID" "MariaDB" || cleanup_rc=71
  if [ "$rc" -eq 0 ] && [ "$cleanup_rc" -ne 0 ]; then
    rc=$cleanup_rc
  fi
  exit "$rc"
}
trap cleanup EXIT INT TERM

# SAFETY: refuse to run outside the private netns - kills below must never
# reach host processes (a production mariadbd lives on this box).
if [ "$(readlink /proc/self/ns/net)" = "$(readlink /proc/1/ns/net)" ]; then
  echo "FATAL: not inside the private netns - aborting"; exit 42
fi
ip link set lo up
choose_port () {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()'
}
PORT_C=$(choose_port)
PORT_R=$(choose_port)
while [ "$PORT_R" -eq "$PORT_C" ]; do PORT_R=$(choose_port); done

# --- throwaway MariaDB (C oracle hardcodes 127.0.0.1:3306/deltamud) ---
mariadb-install-db --datadir="$DBDIR" --auth-root-authentication-method=normal --skip-test-db >/dev/null 2>&1
/usr/sbin/mariadbd --user=root --datadir="$DBDIR" --socket="$SOCK" \
  --port=3306 --bind-address=127.0.0.1 --sql-mode=NO_ENGINE_SUBSTITUTION --skip-ssl --general-log=1 --general-log-file="$WORK/mysql-general.log" --skip-grant-tables --skip-networking=0 \
  --pid-file=$WORK/mysqld.pid >$WORK/mysqld.log 2>&1 &
MYSQL_PID=$!
for i in $(seq 1 100); do
  mariadb --no-defaults --skip-ssl -u root --socket="$SOCK" -e 'SELECT 1' >/dev/null 2>&1 && break
  sleep 0.2
done
reset_db () {
  mariadb --no-defaults --skip-ssl -u root --socket="$SOCK" \
    -e 'DROP DATABASE IF EXISTS deltamud; CREATE DATABASE deltamud'
  mariadb --no-defaults --skip-ssl -u root --socket="$SOCK" deltamud \
    < "$REPO_DIR/deltamud_schema.sql"
  mariadb --no-defaults --skip-ssl -u root --socket="$SOCK" deltamud <<'SQL'
UPDATE player_main
-- The checked-in schema retains the legacy VARCHAR(50) password column, so a
-- modern SHA-crypt hash would be truncated before either server reads it.
-- This full 13-byte DES hash is crypt("pass", "Mu") and both implementations
-- deliberately support it for migration compatibility.
SET name='Mulder', pwd='MuARz2/PsqHFE', host='', hometown=1,
    act=0, clan=-1, clan_rank=-1
WHERE idnum=1;
SQL
}

# --- fresh world copies (never share mutable files between implementations) ---
cp -a "$REPO_DIR/lib" "$LIB_C"
cp -a "$REPO_DIR/lib" "$LIB_R"
# The date_record carries the LAST shutdown's calendar; drop it so both
# servers seed their clock from their respective clean boot and agree.
rm -f "$LIB_C/etc/date_record" "$LIB_R/etc/date_record"

# --- C oracle ---
reset_db
C_ROOT=$WORK/c-root
mkdir -p "$C_ROOT/bin" "$LIB_C/exec"
for b in autowiz scheck licheck; do [ -f "$REPO_DIR/bin/$b" ] && cp "$REPO_DIR/bin/$b" "$C_ROOT/bin/"; done
ln -s "$LIB_C" "$C_ROOT/lib"
: > "$LIB_C/USRCNT"
cd "$C_ROOT"
MYSQL_USER=parity MYSQL_PASSWORD=parity "$C_BIN" -q "$PORT_C" >"$WORK/c.log" 2>&1 &
C_PID=$!

# wait for C, drive it, and stop it before resetting shared external state
for i in $(seq 1 150); do
  (exec 3<>/dev/tcp/127.0.0.1/$PORT_C) 2>/dev/null && break
  sleep 0.2
done
(exec 3<>/dev/tcp/127.0.0.1/$PORT_C) 2>/dev/null || { echo "C oracle did not come up"; tail -20 $WORK/c.log; exit 1; }

python3 "$MUD_DIR/scripts/parity/driver.py" "$PORT_C" "$WORK/raw_c.txt" \
  "$MUD_DIR/scripts/parity/scenario.txt" 2>"$WORK/raw_c.txt.err"
stop_pid "$C_PID" "C oracle"
C_PID=

# --- Rust server, with a newly reset DB and independent lib tree ---
reset_db
cd /tmp
DATABASE_URL=mysql://root@127.0.0.1:3306/deltamud MUD_PORT="$PORT_R" MUD_LIB_PATH="$LIB_R" MUD_RNG_SEED="$SEED" \
  "$RUST_BIN" >"$WORK/r.log" 2>&1 &
R_PID=$!

for i in $(seq 1 150); do
  (exec 3<>/dev/tcp/127.0.0.1/$PORT_R) 2>/dev/null && break
  sleep 0.2
done
(exec 3<>/dev/tcp/127.0.0.1/$PORT_R) 2>/dev/null || { echo "Rust server did not come up"; tail -20 $WORK/r.log; exit 1; }

if [ "${PARITY_FORCE_DRIVER_ERROR:-0}" = "1" ]; then
  python3 "$MUD_DIR/scripts/parity/driver.py" --force-driver-error
fi
python3 "$MUD_DIR/scripts/parity/driver.py" "$PORT_R" "$WORK/raw_r.txt" \
  "$MUD_DIR/scripts/parity/scenario.txt" 2>"$WORK/raw_r.txt.err"

if [ "${PARITY_FORCE_RUST_ZOMBIE:-0}" = "1" ]; then
  kill -KILL "$R_PID"
  for _ in $(seq 1 50); do
    pid_has_exited "$R_PID" && break
    sleep 0.1
  done
fi
stop_pid "$R_PID" "Rust server"
R_PID=
stop_pid "$MYSQL_PID" "MariaDB"
MYSQL_PID=
trap - EXIT INT TERM
exit 0
INNER

normalize () {
  # Strip complete telnet negotiation triplets, ANSI, and CR before making
  # RNG/volatile numbers comparable. Negotiation ordering is protocol-layer
  # noise; its application text must still compare byte-for-byte afterward.
  LC_ALL=C perl -pe 's/\xff[\xfb-\xfe].//g; s/\e\[[0-9;]*[A-Za-z]//g; s/\r//g; s/\d+/N/g' "$1" \
    | grep -av '^\s*$'
}

echo "[parity] booting isolated namespace (mariadb + C oracle + rust)..."
set +e
timeout --signal=TERM --kill-after=15s "$PARITY_TIMEOUT_SECONDS" \
  unshare --fork --kill-child=TERM -n bash "$WORK/netns.sh"
RC=$?
set -e

if [ "$RC" -ne 0 ]; then
  echo "[parity] isolated battery failed (inner rc=$RC); artifacts: $WORK"
  tail -20 "$WORK/c.log" 2>/dev/null || true
  tail -20 "$WORK/r.log" 2>/dev/null || true
  exit "$RC"
fi

for server_log in "$WORK/c.log" "$WORK/r.log"; do
  if grep -Eiq 'PANIC|panicked at|fatal runtime error|stack overflow' "$server_log"; then
    echo "[parity] fatal server marker found in $server_log" >&2
    exit 76
  fi
done

if [ ! -s "$WORK/raw_c.txt" ] || [ ! -s "$WORK/raw_r.txt" ]; then
  echo "[parity] battery did not complete (inner rc=$RC)."
  echo "  driver stderr: $(cat "$WORK/raw_c.txt.err" 2>/dev/null | tail -2)"
  exit 1
fi
for transcript in "$WORK/raw_c.txt" "$WORK/raw_r.txt"; do
  if ! grep -aEq '\[driver\] COMPLETE playing=1 login_steps=[1-9][0-9]* commands=[1-9][0-9]*' "$transcript"; then
    echo "[parity] semantic completion marker missing from $transcript" >&2
    exit 1
  fi
done

normalize "$WORK/raw_c.txt" > "$WORK/norm_c.txt"
normalize "$WORK/raw_r.txt" > "$WORK/norm_r.txt"

if [ "${PROBE:-0}" = "1" ]; then
  echo "[parity] PROBE done. Transcripts:"
  echo "  $WORK/raw_c.txt  ($(wc -l < "$WORK/raw_c.txt") lines)"
  echo "  $WORK/raw_r.txt  ($(wc -l < "$WORK/raw_r.txt") lines)"
  exit 0
fi

set +e
diff -u "$WORK/norm_c.txt" "$WORK/norm_r.txt" > "$WORK/diff.txt"
DIFF_RC=$?
set -e
case "$DIFF_RC" in
  0) LINES=0 ;;
  1) LINES=$(wc -l < "$WORK/diff.txt") ;;
  *) echo "[parity] diff failed (rc=$DIFF_RC)" >&2; exit 75 ;;
esac
echo "[parity] diff lines: $LINES  ($WORK/diff.txt)"
head -80 "$WORK/diff.txt"
[ "$LINES" -eq 0 ] && echo "[parity] CONVERGED; artifacts: $WORK" || exit 1
