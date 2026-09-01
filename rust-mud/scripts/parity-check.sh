#!/usr/bin/env bash
# Parity battery: proves rust-mud against the C oracle side by side.
#
# Isolation: everything runs inside a bubblewrap-created unprivileged user plus
# private network, PID, IPC, and minimal mount namespace
# with its own loopback and a throwaway MariaDB on 127.0.0.1:3306 (the C binary
# hardcodes that endpoint), so neither server can touch production data. The
# world/lib is a fresh copy under an exclusive disk-backed /var/tmp directory
# every run. The
# PID namespace, recursively read-only host filesystem, capability-free oracle
# processes, and PID-only cleanup keep both oracles away from production state.
#
# Usage:
#   cargo build --release --locked
#   RUST_BIN="$PWD/target/release/deltamud" scripts/parity-check.sh
#   PROBE=1 RUST_BIN="$PWD/target/release/deltamud" scripts/parity-check.sh
#
# Inputs:
#   scripts/parity/scenario.txt        # prompt->answer login map + command list
#   scripts/parity/driver.py           # expect-style driver
#
# Outputs in a fresh /var/tmp/deltamud-parity.XXXXXX directory printed at exit:
#   raw_c.txt / raw_r.txt              # raw transcripts
#   norm_c.txt / norm_r.txt            # transport-only normalization
#   diff.txt                           # unified diff (empty == converged)
set -Eeuo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
MUD_DIR=$(cd "$HERE/.." && pwd)
REPO_DIR=$(cd "$MUD_DIR/.." && pwd)
HOST_UID=$(id -u)
if ! awk -v expected="$HOST_UID" '
  /^Uid:/ {
    uid_seen = 1
    if ($2 != expected || $3 != expected || $4 != expected || $5 != expected) bad = 1
  }
  /^Cap(Inh|Prm|Eff|Amb):/ {
    caps_seen++
    if ($2 !~ /^0+$/) bad = 1
  }
  END { exit !(uid_seen && caps_seen == 4 && !bad && expected != 0) }
' "/proc/$$/status"; then
  echo "[parity] refusing host-root, set-ID, or capability-bearing execution of checkout-controlled tools" >&2
  echo "[parity] run as an unprivileged development/CI user; the script maps only that user into the namespace" >&2
  exit 77
fi
[ -f /usr/bin/bwrap ] && [ ! -L /usr/bin/bwrap ] && [ -x /usr/bin/bwrap ] \
  && [ "$(stat -Lc %u -- /usr/bin/bwrap)" -eq 0 ] \
  && [ "$((8#$(stat -Lc %a -- /usr/bin/bwrap) & 8#022))" -eq 0 ] || {
  echo "[parity] a root-owned, non-writable /usr/bin/bwrap is required" >&2
  exit 77
}
if [ -z "${RUST_BIN+x}" ]; then
  echo "[parity] building current Rust source as the invoking unprivileged user..."
  cargo build --release --locked --manifest-path "$MUD_DIR/Cargo.toml"
  RUST_BIN=$MUD_DIR/target/release/deltamud
fi
C_BIN=${C_BIN:-$REPO_DIR/bin/circle}
SEED=${MUD_RNG_SEED:-12345}
PARITY_TIMEOUT_SECONDS=${PARITY_TIMEOUT_SECONDS:-240}
PARITY_FORCE_DRIVER_ERROR=${PARITY_FORCE_DRIVER_ERROR:-0}
PARITY_FORCE_RUST_ZOMBIE=${PARITY_FORCE_RUST_ZOMBIE:-0}
PARITY_FORCE_CHILD_LEAK=${PARITY_FORCE_CHILD_LEAK:-0}

if [ ! -x "$RUST_BIN" ] || [ ! -x "$C_BIN" ]; then
  echo "[parity] required executable missing (Rust: $RUST_BIN; C: $C_BIN)" >&2
  exit 65
fi
if ! [[ "$PARITY_TIMEOUT_SECONDS" =~ ^[1-9][0-9]*$ ]]; then
  echo "[parity] PARITY_TIMEOUT_SECONDS must be a positive integer" >&2
  exit 64
fi
if [[ "$PARITY_FORCE_DRIVER_ERROR" != 0 && "$PARITY_FORCE_DRIVER_ERROR" != 1 ]] \
  || [[ "$PARITY_FORCE_RUST_ZOMBIE" != 0 && "$PARITY_FORCE_RUST_ZOMBIE" != 1 ]] \
  || [[ "$PARITY_FORCE_CHILD_LEAK" != 0 && "$PARITY_FORCE_CHILD_LEAK" != 1 ]]; then
  echo "[parity] negative-control flags must be 0 or 1" >&2
  exit 64
fi
RUST_BIN=$(readlink -f -- "$RUST_BIN")
C_BIN=$(readlink -f -- "$C_BIN")

umask 077
WORK=$(mktemp -d /var/tmp/deltamud-parity.XXXXXX)
[ -d "$WORK" ] && [ ! -L "$WORK" ] \
  && [ "$(stat -Lc %u:%g:%a -- "$WORK")" = "$(id -u):$(id -g):700" ] || {
    echo "[parity] could not create a private invoking-user-owned work directory" >&2
    exit 77
  }
install -d -m 0755 "$WORK/bin" "$WORK/input/rust-mud/scripts/parity" "$WORK/input/bin"
install -d -m 0700 "$WORK/tmp"
rust_before=$(sha256sum -- "$RUST_BIN" | awk '{print $1}')
c_before=$(sha256sum -- "$C_BIN" | awk '{print $1}')
install -m 0755 "$RUST_BIN" "$WORK/bin/deltamud"
install -m 0755 "$C_BIN" "$WORK/bin/circle"
[ "$rust_before" = "$(sha256sum -- "$RUST_BIN" | awk '{print $1}')" ] \
  && [ "$rust_before" = "$(sha256sum -- "$WORK/bin/deltamud" | awk '{print $1}')" ] \
  && [ "$c_before" = "$(sha256sum -- "$C_BIN" | awk '{print $1}')" ] \
  && [ "$c_before" = "$(sha256sum -- "$WORK/bin/circle" | awk '{print $1}')" ] || {
    echo "[parity] an oracle executable changed while entering the private stage" >&2
    exit 65
  }
RUST_BIN=$WORK/bin/deltamud
C_BIN=$WORK/bin/circle
export PARITY_RUST_SHA256=$rust_before
export PARITY_C_SHA256=$c_before
cp -R -- "$REPO_DIR/lib" "$WORK/input/lib"
install -m 0644 "$REPO_DIR/deltamud_schema.sql" "$WORK/input/deltamud_schema.sql"
install -m 0644 "$MUD_DIR/scripts/parity/driver.py" \
  "$MUD_DIR/scripts/parity/scenario.txt" "$WORK/input/rust-mud/scripts/parity/"
for helper in autowiz scheck licheck; do
  [ ! -f "$REPO_DIR/bin/$helper" ] \
    || install -m 0755 "$REPO_DIR/bin/$helper" "$WORK/input/bin/$helper"
done
if find "$WORK/input" \! -type d \! -type f -print -quit | grep -q .; then
  echo "[parity] staged inputs contain a link or special file" >&2
  exit 65
fi
chmod -R a-w "$WORK/input"
export PARITY_WORK="$WORK"
export PARITY_RUST_BIN="$RUST_BIN"
export PARITY_C_BIN="$C_BIN"
export PARITY_MUD_DIR="$WORK/input/rust-mud"
export PARITY_REPO_DIR="$WORK/input"
export PARITY_HOST_NETNS
PARITY_HOST_NETNS=$(readlink /proc/self/ns/net)

# The inner script uses a QUOTED heredoc marker ('INNER') so nothing is
# expanded by this outer shell: every $var is evaluated by the inner shell.
set -o noclobber
cat > "$WORK/netns.sh" <<'INNER'
set -Eeuo pipefail
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
WORK=${PARITY_WORK:?missing parity work directory}
export TMPDIR=$WORK/tmp
SEED=${MUD_RNG_SEED:-12345}
RUST_BIN=${PARITY_RUST_BIN:?missing Rust binary}
C_BIN=${PARITY_C_BIN:?missing C binary}
MUD_DIR=${PARITY_MUD_DIR:?missing Rust MUD directory}
REPO_DIR=${PARITY_REPO_DIR:?missing repository directory}
HOST_NETNS=${PARITY_HOST_NETNS:?missing host network namespace identity}
RUST_SHA256=${PARITY_RUST_SHA256:?missing Rust oracle digest}
C_SHA256=${PARITY_C_SHA256:?missing C oracle digest}
LIB_C=$WORK/lib-c
LIB_R=$WORK/lib-r
DBDIR=$WORK/mariadb
MYSQL_RUN=$WORK/mysql-run
SOCK=$MYSQL_RUN/mariadb.sock
MYSQL_PID=
C_PID=
R_PID=
command -v setpriv >/dev/null 2>&1 || { echo "missing setpriv privilege-drop tool"; exit 77; }
ORACLE_PREFIX=(
  setpriv --no-new-privs --inh-caps=-all --ambient-caps=-all
  --pdeathsig SIGKILL
)
ORACLE_ENV=(
  env -i PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
  HOME="$WORK" TMPDIR="$TMPDIR" LANG=C.UTF-8 LC_ALL=C.UTF-8 TZ=UTC
)
[ "$(id -u)" -eq 1 ] || { echo "unexpected parity namespace identity"; exit 77; }
awk '
  /^Uid:/ { seen_uid = 1; if ($2 != 1 || $3 != 1 || $4 != 1 || $5 != 1) bad = 1 }
  /^Cap(Inh|Prm|Eff|Bnd|Amb):/ { seen_caps++; if ($2 !~ /^0+$/) bad = 1 }
  END { exit !(seen_uid && seen_caps == 5 && !bad) }
' /proc/$$/status || { echo "parity namespace retained an identity or capability"; exit 77; }

# Bubblewrap supplies private /dev, /proc, and /run plus a disk-backed private /tmp,
# then exposes only system executables/config read-only plus this run's work
# tree. The staged control/input/binary subtrees are additional read-only
# mounts. Check the effective topology and prove the parser rejects an
# unexpected writable host-shaped mount before starting any oracle.
cp -- /proc/self/mountinfo "$WORK/mount-options.after"
verify_only_private_mounts_are_writable () {
  local inventory=$1
  local mount_id parent_id device mount_root mount_target mount_options remainder
  local records=0
  while IFS=' ' read -r mount_id parent_id device mount_root mount_target \
    mount_options remainder; do
    records=$((records + 1))
    case ",$mount_options," in
      *,rw,*)
        case "$mount_target" in
          /|/work|/tmp|/run|/dev|/dev/*|/proc|/proc/*) ;;
          *) echo "unexpected writable mount: $mount_target" >&2; return 1 ;;
        esac
        ;;
    esac
  done <"$inventory"
  [ "$records" -gt 0 ]
}
# Negative control: prove that the parser rejects an unexpected writable mount,
# rather than merely accepting a malformed or empty inventory.
printf '%s\n' \
  '1 0 0:1 / / rw - tmpfs tmpfs rw' \
  '2 1 0:2 / /host rw,nosuid - tmpfs tmpfs rw,nosuid' \
  >"$WORK/mount-options.negative"
if verify_only_private_mounts_are_writable "$WORK/mount-options.negative" 2>/dev/null; then
  echo "mount isolation verifier accepted its writable-mount negative control" >&2
  exit 77
fi
if ! verify_only_private_mounts_are_writable "$WORK/mount-options.after"; then
  echo "a non-private mount remains writable in the parity sandbox" >&2
  exit 77
fi
for protected_target in /work/input /work/bin /work/netns.sh; do
  awk -v target="$protected_target" '
    $5 == target && ("," $6 ",") ~ /,ro,/ { found = 1 }
    END { exit found ? 0 : 1 }
  ' /proc/self/mountinfo || {
    echo "protected parity mount is not read-only: $protected_target" >&2
    exit 77
  }
done
if find /run /tmp /dev -type s -print -quit | grep -q .; then
  echo "the parity sandbox inherited a host Unix-domain socket" >&2
  exit 77
fi
verify_oracle_hash () {
  local binary=$1
  local expected=$2
  local label=$3
  [ "$(sha256sum -- "$binary" | awk '{print $1}')" = "$expected" ] || {
    echo "$label oracle changed inside the parity namespace" >&2
    return 1
  }
}
mkdir -p "$MYSQL_RUN"
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
    15:0|15:143) return 0 ;;
    9:137)
      echo "[parity] $label required forced SIGKILL after its shutdown deadline" >&2
      return 71
      ;;
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
  for proc_dir in /proc/[0-9]*; do
    [ "${proc_dir##*/}" = "$$" ] && continue
    if [ "${proc_dir##*/}" = 1 ]; then
      init_argv0=
      IFS= read -r -d '' init_argv0 <"$proc_dir/cmdline" 2>/dev/null || true
      if [ "$init_argv0" = /usr/bin/bwrap ] \
        && [ "$(readlink -e "$proc_dir/exe" 2>/dev/null || true)" = /usr/bin/bwrap ] \
        && awk '
          /^Name:/ { name_seen = 1; if ($2 != "bwrap") bad = 1 }
          /^State:/ { state_seen = 1; if ($2 ~ /^Z/) bad = 1 }
          /^PPid:/ { ppid_seen = 1; if ($2 != 0) bad = 1 }
          /^Uid:/ {
            uid_seen = 1
            if ($2 != 1 || $3 != 1 || $4 != 1 || $5 != 1) bad = 1
          }
          /^Cap(Inh|Prm|Eff|Bnd|Amb):/ { caps_seen++; if ($2 !~ /^0+$/) bad = 1 }
          END {
            exit !(name_seen && state_seen && ppid_seen && uid_seen \
              && caps_seen == 5 && !bad)
          }
        ' "$proc_dir/status"; then
        continue
      fi
    fi
    echo "[parity] untracked PID ${proc_dir##*/} remains in the private PID namespace" >&2
    cleanup_rc=71
  done
  if [ "$rc" -eq 0 ] && [ "$cleanup_rc" -ne 0 ]; then
    rc=$cleanup_rc
  fi
  exit "$rc"
}
trap cleanup EXIT INT TERM

# SAFETY: refuse to run outside the private netns - kills below must never
# reach host processes (a production mariadbd lives on this box).
if [ "$(readlink /proc/self/ns/net)" = "$HOST_NETNS" ]; then
  echo "FATAL: not inside the private netns - aborting"; exit 42
fi
ip -o link show lo | grep -Eq '<[^>]*UP' || {
  echo "FATAL: bubblewrap private loopback is not up"; exit 42
}
choose_port () {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()'
}
PORT_C=$(choose_port)
PORT_R=$(choose_port)
while [ "$PORT_R" -eq "$PORT_C" ]; do PORT_R=$(choose_port); done

# --- throwaway MariaDB (C oracle hardcodes 127.0.0.1:3306/deltamud) ---
mariadb-install-db --no-defaults --datadir="$DBDIR" \
  --auth-root-authentication-method=normal --skip-test-db >/dev/null 2>&1
"${ORACLE_PREFIX[@]}" "${ORACLE_ENV[@]}" \
  /usr/sbin/mariadbd --no-defaults --datadir="$DBDIR" --socket="$SOCK" \
  --tmpdir="$TMPDIR" \
  --port=3306 --bind-address=127.0.0.1 --sql-mode=NO_ENGINE_SUBSTITUTION --skip-ssl \
  --general-log=1 --general-log-file="$MYSQL_RUN/mysql-general.log" --skip-grant-tables \
  --skip-networking=0 --pid-file="$MYSQL_RUN/mysqld.pid" >$WORK/mysqld.log 2>&1 &
MYSQL_PID=$!
for i in $(seq 1 100); do
  "${ORACLE_PREFIX[@]}" "${ORACLE_ENV[@]}" mariadb --no-defaults --skip-ssl -u root --socket="$SOCK" \
    -e 'SELECT 1' >/dev/null 2>&1 && break
  sleep 0.2
done
reset_db () {
  "${ORACLE_PREFIX[@]}" "${ORACLE_ENV[@]}" mariadb --no-defaults --skip-ssl -u root --socket="$SOCK" \
    -e 'DROP DATABASE IF EXISTS deltamud; CREATE DATABASE deltamud'
  "${ORACLE_PREFIX[@]}" "${ORACLE_ENV[@]}" mariadb --no-defaults --binary-mode --skip-ssl -u root \
    --socket="$SOCK" deltamud \
    < "$REPO_DIR/deltamud_schema.sql"
  "${ORACLE_PREFIX[@]}" "${ORACLE_ENV[@]}" mariadb --no-defaults --skip-ssl -u root \
    --socket="$SOCK" deltamud <<'SQL'
-- Parity owns its fixture explicitly: the shipped schema is deliberately
-- empty and production bootstrap never creates a known administrative login.
-- This full DES hash is crypt("pass", "Mu"); both implementations retain it
-- only for legacy-login migration compatibility.
INSERT INTO player_main (
  idnum, name, pwd, level, sex, class, race, deity, hometown, birth, played,
  last_logon, host, hit, max_hit, mana, max_mana, move, max_move, gold, exp,
  str, intel, wis, dex, con, cha, alignment, load_room, act, clan, clan_rank,
  trust, godcmds1, godcmds2, godcmds3, godcmds4
) VALUES (
  1, 'Mulder', 'MuARz2/PsqHFE', 60, 1, 0, 0, 0, 1, UNIX_TIMESTAMP(), 0,
  UNIX_TIMESTAMP(), '', 500, 500, 100, 100, 100, 100, 50000, 0,
  18, 18, 18, 18, 18, 18, 0, 0, 0, -1, -1,
  0, 0, 0, 0, 0
);
SQL
}

# --- fresh world copies (never share mutable files between implementations) ---
cp -a "$REPO_DIR/lib" "$LIB_C"
cp -a "$REPO_DIR/lib" "$LIB_R"
chmod -R u+rwX "$LIB_C" "$LIB_R"
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
verify_oracle_hash "$C_BIN" "$C_SHA256" C
verify_oracle_hash "$RUST_BIN" "$RUST_SHA256" Rust
"${ORACLE_PREFIX[@]}" "${ORACLE_ENV[@]}" \
  MYSQL_USER=parity MYSQL_PASSWORD=parity \
  "$C_BIN" -q "$PORT_C" >"$WORK/c.log" 2>&1 &
C_PID=$!

# wait for C, drive it, and stop it before resetting shared external state
for i in $(seq 1 150); do
  (exec 3<>/dev/tcp/127.0.0.1/$PORT_C) 2>/dev/null && break
  sleep 0.2
done
(exec 3<>/dev/tcp/127.0.0.1/$PORT_C) 2>/dev/null || { echo "C oracle did not come up"; tail -20 $WORK/c.log; exit 1; }

"${ORACLE_PREFIX[@]}" "${ORACLE_ENV[@]}" python3 "$MUD_DIR/scripts/parity/driver.py" \
  "$PORT_C" "$WORK/raw_c.txt" \
  "$MUD_DIR/scripts/parity/scenario.txt" 2>"$WORK/raw_c.txt.err"
stop_pid "$C_PID" "C oracle"
C_PID=
verify_oracle_hash "$C_BIN" "$C_SHA256" C
verify_oracle_hash "$RUST_BIN" "$RUST_SHA256" Rust

# --- Rust server, with a newly reset DB and independent lib tree ---
reset_db
cd "$WORK"
verify_oracle_hash "$RUST_BIN" "$RUST_SHA256" Rust
"${ORACLE_PREFIX[@]}" "${ORACLE_ENV[@]}" \
  DATABASE_URL=mysql://root@127.0.0.1:3306/deltamud MUD_MOCK_DB=0 \
  "$RUST_BIN" --migrate >"$WORK/r-migrate.log" 2>&1 || {
    echo "Rust schema migration failed"
    tail -20 "$WORK/r-migrate.log"
    exit 1
  }
verify_oracle_hash "$RUST_BIN" "$RUST_SHA256" Rust
"${ORACLE_PREFIX[@]}" "${ORACLE_ENV[@]}" \
  DATABASE_URL=mysql://root@127.0.0.1:3306/deltamud MUD_MOCK_DB=0 \
  MUD_BIND=127.0.0.1 MUD_PORT="$PORT_R" MUD_LIB_PATH="$LIB_R" MUD_RNG_SEED="$SEED" \
  "$RUST_BIN" >"$WORK/r.log" 2>&1 &
R_PID=$!

for i in $(seq 1 150); do
  (exec 3<>/dev/tcp/127.0.0.1/$PORT_R) 2>/dev/null && break
  sleep 0.2
done
(exec 3<>/dev/tcp/127.0.0.1/$PORT_R) 2>/dev/null || { echo "Rust server did not come up"; tail -20 $WORK/r.log; exit 1; }

if [ "${PARITY_FORCE_DRIVER_ERROR:-0}" = "1" ]; then
  "${ORACLE_PREFIX[@]}" "${ORACLE_ENV[@]}" python3 "$MUD_DIR/scripts/parity/driver.py" \
    --force-driver-error
fi
"${ORACLE_PREFIX[@]}" "${ORACLE_ENV[@]}" python3 "$MUD_DIR/scripts/parity/driver.py" \
  "$PORT_R" "$WORK/raw_r.txt" \
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
verify_oracle_hash "$C_BIN" "$C_SHA256" C
verify_oracle_hash "$RUST_BIN" "$RUST_SHA256" Rust
if [ "${PARITY_FORCE_CHILD_LEAK:-0}" = "1" ]; then
  sleep 300 &
  echo "[parity-negative] injected untracked PID $!" >&2
fi
exit 0
INNER
chmod 0700 "$WORK/netns.sh"

normalize () {
  # Strip only complete Telnet negotiation triplets, ANSI, and CR. Every game
  # number (levels, vnums, stats, prices, dates) remains parity evidence.
  LC_ALL=C perl -pe 's/\xff[\xfb-\xfe].//g; s/\e\[[0-9;]*[A-Za-z]//g; s/\r//g' "$1" \
    | grep -av '^\s*$'
}

echo "[parity] booting isolated namespace (mariadb + C oracle + rust)..."
set +e
env -i PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  HOME="$WORK" TMPDIR="$WORK/tmp" LANG=C.UTF-8 LC_ALL=C.UTF-8 TZ=UTC \
  timeout --signal=TERM --kill-after=15s "$PARITY_TIMEOUT_SECONDS" \
  /usr/bin/bwrap --unshare-all --new-session --die-with-parent --uid 1 --gid 1 \
  --cap-drop ALL --proc /proc --dev /dev --tmpfs /run \
  --ro-bind /usr /usr --symlink usr/bin /bin --symlink usr/sbin /sbin \
  --symlink usr/lib /lib --symlink usr/lib64 /lib64 --dir /etc \
  --ro-bind /etc/alternatives /etc/alternatives \
  --ro-bind /etc/ld.so.cache /etc/ld.so.cache \
  --ro-bind /etc/passwd /etc/passwd --ro-bind /etc/group /etc/group \
  --ro-bind /etc/nsswitch.conf /etc/nsswitch.conf \
  --ro-bind /etc/hosts /etc/hosts --ro-bind /etc/resolv.conf /etc/resolv.conf \
  --dir /var --dir /var/tmp --bind "$WORK" /work \
  --bind "$WORK/tmp" /tmp \
  --ro-bind "$WORK/bin" /work/bin --ro-bind "$WORK/input" /work/input \
  --ro-bind "$WORK/netns.sh" /work/netns.sh --chdir /work \
  --clearenv --setenv PATH /usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  --setenv HOME /work --setenv TMPDIR /work/tmp --setenv LANG C.UTF-8 \
  --setenv LC_ALL C.UTF-8 --setenv TZ UTC \
  --setenv PARITY_WORK /work --setenv PARITY_RUST_BIN /work/bin/deltamud \
  --setenv PARITY_C_BIN /work/bin/circle \
  --setenv PARITY_MUD_DIR /work/input/rust-mud \
  --setenv PARITY_REPO_DIR /work/input \
  --setenv PARITY_HOST_NETNS "$PARITY_HOST_NETNS" \
  --setenv PARITY_RUST_SHA256 "$PARITY_RUST_SHA256" \
  --setenv PARITY_C_SHA256 "$PARITY_C_SHA256" --setenv MUD_RNG_SEED "$SEED" \
  --setenv PARITY_FORCE_DRIVER_ERROR "$PARITY_FORCE_DRIVER_ERROR" \
  --setenv PARITY_FORCE_RUST_ZOMBIE "$PARITY_FORCE_RUST_ZOMBIE" \
  --setenv PARITY_FORCE_CHILD_LEAK "$PARITY_FORCE_CHILD_LEAK" \
  /bin/bash /work/netns.sh
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
