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
# Outputs in /tmp/parity-run/:
#   raw_c.txt / raw_r.txt              # raw transcripts
#   norm_c.txt / norm_r.txt            # normalized (ANSI stripped, digits->N)
#   diff.txt                           # unified diff (empty == converged)
set -u

RUST_BIN=${RUST_BIN:-/web/deltamud/rust-mud/target/release/deltamud}
C_BIN=${C_BIN:-/web/deltamud/bin/circle}
HERE=$(cd "$(dirname "$0")" && pwd)
LIB=/tmp/parity-lib
WORK=/tmp/parity-run
PORT_C=4100
PORT_R=4000
SEED=${MUD_RNG_SEED:-12345}
SCHEMA=${SCHEMA:-/tmp/parity-schema.sql}

mkdir -p "$WORK" "$HERE/parity"
rm -f "$WORK/raw_c.txt" "$WORK/raw_r.txt" "$WORK/"*.err "$WORK/diff.txt"

# The inner script uses a QUOTED heredoc marker ('INNER') so nothing is
# expanded by this outer shell: every $var is evaluated by the inner shell.
cat > "$WORK/netns.sh" <<'INNER'
set -u
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
WORK=/tmp/parity-run
PORT_C=4100
PORT_R=4000
SEED=${MUD_RNG_SEED:-12345}
LIB=/tmp/parity-lib

# SAFETY: refuse to run outside the private netns - kills below must never
# reach host processes (a production mariadbd lives on this box).
if [ "$(readlink /proc/self/ns/net)" = "$(readlink /proc/1/ns/net)" ]; then
  echo "FATAL: not inside the private netns - aborting"; exit 42
fi
ip link set lo up

# --- throwaway MariaDB (C oracle hardcodes 127.0.0.1:3306/deltamud) ---
rm -rf /tmp/parity-db /tmp/parity.sock
mariadb-install-db --datadir=/tmp/parity-db --auth-root-authentication-method=normal --skip-test-db >/dev/null 2>&1
/usr/sbin/mariadbd --user=root --datadir=/tmp/parity-db --socket=/tmp/parity.sock \
  --port=3306 --bind-address=127.0.0.1 --sql-mode=NO_ENGINE_SUBSTITUTION --skip-ssl --general-log=1 --general-log-file=/tmp/parity-run/mysql-general.log --skip-grant-tables --skip-networking=0 \
  --pid-file=$WORK/mysqld.pid >$WORK/mysqld.log 2>&1 &
MYSQL_PID=$!
for i in $(seq 1 100); do
  mariadb --no-defaults --skip-ssl -u root --socket=/tmp/parity.sock -e 'SELECT 1' >/dev/null 2>&1 && break
  sleep 0.2
done
mariadb --no-defaults --skip-ssl -u root --socket=/tmp/parity.sock -e 'CREATE DATABASE deltamud' || exit 1
mariadb --no-defaults --skip-ssl -u root --socket=/tmp/parity.sock deltamud < /tmp/parity-schema.sql || exit 1
mariadb --no-defaults --skip-ssl -u root --socket=/tmp/parity.sock deltamud < /tmp/parity-seed.sql || exit 1

# --- fresh world copy (never share runtime files with the live lib) ---
rm -rf $LIB
cp -a /web/deltamud/lib $LIB
# The date_record carries the LAST shutdown's calendar; drop it so both
# servers seed their clock from this boot (seconds apart) and agree.
rm -f $LIB/etc/date_record

# --- C oracle ---
mkdir -p /tmp/parity-c/bin
mkdir -p /tmp/parity-c/lib/exec
mkdir -p /tmp/parity-c/bin /tmp/parity-c/lib/exec
for b in autowiz scheck licheck; do [ -f /web/deltamud/bin/$b ] && cp /web/deltamud/bin/$b /tmp/parity-c/bin/; done
ln -sfn $LIB /tmp/parity-c/lib
: > $LIB/USRCNT
cd /tmp/parity-c
MYSQL_USER=parity MYSQL_PASSWORD=parity /web/deltamud/bin/circle -q $PORT_C >$WORK/c.log 2>&1 &
C_PID=$!

# --- Rust server ---
cd /tmp
DATABASE_URL=mysql://root@127.0.0.1:3306/deltamud MUD_PORT=$PORT_R MUD_LIB_PATH=$LIB MUD_RNG_SEED=$SEED \
  /web/deltamud/rust-mud/target/release/deltamud >$WORK/r.log 2>&1 &
R_PID=$!

# wait for both listeners
for i in $(seq 1 150); do
  (exec 3<>/dev/tcp/127.0.0.1/$PORT_C) 2>/dev/null && break
  sleep 0.2
done
(exec 3<>/dev/tcp/127.0.0.1/$PORT_C) 2>/dev/null || { echo "C oracle did not come up"; tail -20 $WORK/c.log; exit 1; }
for i in $(seq 1 150); do
  (exec 3<>/dev/tcp/127.0.0.1/$PORT_R) 2>/dev/null && break
  sleep 0.2
done
(exec 3<>/dev/tcp/127.0.0.1/$PORT_R) 2>/dev/null || { echo "Rust server did not come up"; tail -20 $WORK/r.log; exit 1; }

python3 /web/deltamud/rust-mud/scripts/parity/driver.py $PORT_C $WORK/raw_c.txt \
  /web/deltamud/rust-mud/scripts/parity/scenario.txt 2>$WORK/raw_c.txt.err
python3 /web/deltamud/rust-mud/scripts/parity/driver.py $PORT_R $WORK/raw_r.txt \
  /web/deltamud/rust-mud/scripts/parity/scenario.txt 2>$WORK/raw_r.txt.err

kill $C_PID $R_PID $MYSQL_PID 2>/dev/null
wait $C_PID $R_PID $MYSQL_PID 2>/dev/null
exit 0
INNER

normalize () {
  # strip ANSI + IAC noise + CR; make RNG/volatile numbers comparable
  perl -pe 's/\e\[[0-9;]*[A-Za-z]//g; s/\xff/\n/g; s/\r//g; s/\d+/N/g' "$1" \
    | grep -av '^\s*$'
}

echo "[parity] booting isolated namespace (mariadb + C oracle + rust)..."
unshare -n bash "$WORK/netns.sh"
RC=$?

if [ ! -s "$WORK/raw_c.txt" ] || [ ! -s "$WORK/raw_r.txt" ]; then
  echo "[parity] battery did not complete (inner rc=$RC)."
  echo "  driver stderr: $(cat "$WORK/raw_c.txt.err" 2>/dev/null | tail -2)"
  exit 1
fi

normalize "$WORK/raw_c.txt" > "$WORK/norm_c.txt"
normalize "$WORK/raw_r.txt" > "$WORK/norm_r.txt"

if [ "${PROBE:-0}" = "1" ]; then
  echo "[parity] PROBE done. Transcripts:"
  echo "  $WORK/raw_c.txt  ($(wc -l < "$WORK/raw_c.txt") lines)"
  echo "  $WORK/raw_r.txt  ($(wc -l < "$WORK/raw_r.txt") lines)"
  exit 0
fi

diff -u "$WORK/norm_c.txt" "$WORK/norm_r.txt" > "$WORK/diff.txt"
LINES=$(wc -l < "$WORK/diff.txt")
echo "[parity] diff lines: $LINES  ($WORK/diff.txt)"
head -80 "$WORK/diff.txt"
[ "$LINES" -eq 0 ] && echo "[parity] CONVERGED" || exit 1
