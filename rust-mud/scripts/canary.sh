#!/usr/bin/env bash
# Isolated, fail-closed DeltaMUD live canary runner.
set -Eeuo pipefail

CANARY_ARGS=("$@")
SECONDS_TO_RUN=8
PLAYERS=1
ARTIFACTS=
NEGATIVE_CONTROL=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --seconds) SECONDS_TO_RUN=$2; shift 2 ;;
    --players) PLAYERS=$2; shift 2 ;;
    --artifacts) ARTIFACTS=$2; shift 2 ;;
    --negative-control) NEGATIVE_CONTROL=$2; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 64 ;;
  esac
done

if ! [[ "$SECONDS_TO_RUN" =~ ^[1-9][0-9]*$ ]] || ! [[ "$PLAYERS" =~ ^[1-8]$ ]]; then
  echo "[canary] seconds must be positive and players must be 1..8" >&2
  exit 64
fi

# Keep the whole canary, including boot and EXIT-trap cleanup, under one hard
# deadline. The inner soak timeout remains a tighter bound for its own driver.
CANARY_HARD_TIMEOUT_SECONDS=${CANARY_HARD_TIMEOUT_SECONDS:-$((SECONDS_TO_RUN + 180))}
CANARY_KILL_AFTER_SECONDS=${CANARY_KILL_AFTER_SECONDS:-15}
if ! [[ "$CANARY_HARD_TIMEOUT_SECONDS" =~ ^[1-9][0-9]*$ ]] ||
   ! [[ "$CANARY_KILL_AFTER_SECONDS" =~ ^[1-9][0-9]*$ ]]; then
  echo "[canary] hard timeout and kill-after must be positive integers" >&2
  exit 64
fi
if [ "${DELTAMUD_CANARY_TIMEOUT_PARENT:-}" != "$PPID" ]; then
  export DELTAMUD_CANARY_TIMEOUT_PARENT=$$
  exec timeout --signal=TERM --kill-after="$CANARY_KILL_AFTER_SECONDS" \
    "$CANARY_HARD_TIMEOUT_SECONDS" bash "$0" "${CANARY_ARGS[@]}"
fi
unset DELTAMUD_CANARY_TIMEOUT_PARENT

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
MUD_DIR=$(cd "$SCRIPT_DIR/.." && pwd)
REPO_DIR=$(cd "$MUD_DIR/.." && pwd)
BINARY=${RUST_BIN:-$MUD_DIR/target/release/deltamud}
if [ ! -x "$BINARY" ]; then
  echo "[canary] release binary missing: $BINARY" >&2
  exit 65
fi

if [ -z "$ARTIFACTS" ]; then
  ARTIFACTS="$MUD_DIR/canary-artifacts"
fi
mkdir -p "$ARTIFACTS"
CANARY_DIR=$(mktemp -d /tmp/deltamud-canary.XXXXXX)
SERVER_PID=
SERVER_STOPPED=0

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
    if pid_has_exited "$pid"; then
      return 0
    fi
    sleep 0.1
  done
  pid_has_exited "$pid"
}

cleanup () {
  rc=$?
  cleanup_rc=0
  trap - EXIT INT TERM
  set +e
  if [ -n "$SERVER_PID" ]; then
    if ! pid_has_exited "$SERVER_PID"; then
      if [ "$SERVER_STOPPED" -eq 1 ]; then
        if ! kill -CONT "$SERVER_PID" 2>/dev/null && ! pid_has_exited "$SERVER_PID"; then
          cleanup_rc=71
        fi
      fi
      if ! kill -TERM "$SERVER_PID" 2>/dev/null && ! pid_has_exited "$SERVER_PID"; then
        cleanup_rc=71
      fi
    fi
    if ! wait_for_pid_exit "$SERVER_PID" 50; then
      if ! kill -KILL "$SERVER_PID" 2>/dev/null && ! pid_has_exited "$SERVER_PID"; then
        cleanup_rc=71
      fi
      if ! wait_for_pid_exit "$SERVER_PID" 50; then
        echo "[canary] server PID $SERVER_PID survived SIGKILL cleanup deadline" >&2
        cleanup_rc=71
      fi
    fi
    if pid_has_exited "$SERVER_PID"; then
      wait "$SERVER_PID" 2>/dev/null || true
      SERVER_PID=
    fi
  fi
  if [ -f "$CANARY_DIR/server.log" ]; then
    cp "$CANARY_DIR/server.log" "$ARTIFACTS/server.log" || cleanup_rc=72
  fi
  if [ -f "$CANARY_DIR/metrics.before" ]; then
    cp "$CANARY_DIR/metrics.before" "$ARTIFACTS/metrics.before" || cleanup_rc=72
  fi
  if [ -f "$CANARY_DIR/metrics.after" ]; then
    cp "$CANARY_DIR/metrics.after" "$ARTIFACTS/metrics.after" || cleanup_rc=72
  fi
  rm -rf -- "$CANARY_DIR" || cleanup_rc=73
  if [ "$rc" -eq 0 ] && [ "$cleanup_rc" -ne 0 ]; then
    rc=$cleanup_rc
  fi
  exit "$rc"
}
trap cleanup EXIT INT TERM

mkdir -p "$CANARY_DIR/lib"
cp -a "$REPO_DIR/lib/." "$CANARY_DIR/lib/"
rm -rf -- "$CANARY_DIR/lib/plrobjs" "$CANARY_DIR/lib/plralias"
rm -f -- "$CANARY_DIR/lib/etc/date_record" "$CANARY_DIR/lib/USRCNT"
mkdir -p "$CANARY_DIR/lib/plrobjs" "$CANARY_DIR/lib/plralias"

choose_port () {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()'
}
GAME_PORT=$(choose_port)
METRICS_PORT=$(choose_port)
while [ "$METRICS_PORT" -eq "$GAME_PORT" ]; do METRICS_PORT=$(choose_port); done

MUD_MOCK_DB=true \
MUD_PORT="$GAME_PORT" \
MUD_METRICS_PORT="$METRICS_PORT" \
MUD_LIB_PATH="$CANARY_DIR/lib" \
MUD_RNG_SEED=424242 \
"$BINARY" >"$CANARY_DIR/server.log" 2>&1 &
SERVER_PID=$!

ready=0
for _ in $(seq 1 200); do
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "[canary] server exited during boot" >&2
    exit 66
  fi
  if curl --fail --silent --max-time 2 "http://127.0.0.1:$METRICS_PORT/health" >/dev/null; then
    ready=1
    break
  fi
  sleep 0.1
done
if [ "$ready" -ne 1 ]; then
  echo "[canary] health endpoint did not become ready" >&2
  exit 67
fi

case "$NEGATIVE_CONTROL" in
  "") ;;
  kill-server)
    kill -KILL "$SERVER_PID"
    if ! wait_for_pid_exit "$SERVER_PID" 50; then
      echo "[canary] injected server death exceeded cleanup deadline" >&2
      exit 71
    fi
    wait "$SERVER_PID" 2>/dev/null || true
    SERVER_PID=
    echo "[canary] injected server death" >&2
    ;;
  freeze-pulses)
    kill -STOP "$SERVER_PID"
    SERVER_STOPPED=1
    sleep 1
    echo "[canary] injected frozen heartbeat" >&2
    ;;
  driver) ;;
  *) echo "[canary] unknown negative control: $NEGATIVE_CONTROL" >&2; exit 64 ;;
esac

curl --fail --silent --max-time 2 "http://127.0.0.1:$METRICS_PORT/metrics" >"$CANARY_DIR/metrics.before"
PULSE_BEFORE=$(awk '$1 == "deltamud_pulse" {print $2}' "$CANARY_DIR/metrics.before")
sleep 1
curl --fail --silent --max-time 2 "http://127.0.0.1:$METRICS_PORT/metrics" >"$CANARY_DIR/metrics.after"
PULSE_AFTER=$(awk '$1 == "deltamud_pulse" {print $2}' "$CANARY_DIR/metrics.after")
if ! [[ "$PULSE_BEFORE" =~ ^[0-9]+$ ]] || ! [[ "$PULSE_AFTER" =~ ^[0-9]+$ ]]; then
  echo "[canary] heartbeat metric must contain exactly one unsigned integer sample" >&2
  exit 68
fi
if [ "$PULSE_AFTER" -le "$PULSE_BEFORE" ]; then
  echo "[canary] heartbeat pulse did not advance" >&2
  exit 68
fi

DRIVER_EXTRA=()
if [ "$NEGATIVE_CONTROL" = "driver" ]; then
  DRIVER_EXTRA+=(--force-driver-error)
fi
timeout --foreground --signal=TERM --kill-after=5 "$((SECONDS_TO_RUN + 90))" \
  python3 "$SCRIPT_DIR/soak_combat.py" \
    --port "$GAME_PORT" \
    --health "$METRICS_PORT" \
    --players "$PLAYERS" \
    --seconds "$SECONDS_TO_RUN" \
    --log "$CANARY_DIR/server.log" \
    --artifacts "$ARTIFACTS" \
    "${DRIVER_EXTRA[@]}"

if ! kill -0 "$SERVER_PID" 2>/dev/null; then
  echo "[canary] server exited during the live workload" >&2
  exit 69
fi
curl --fail --silent --max-time 2 "http://127.0.0.1:$METRICS_PORT/health" >"$ARTIFACTS/health.after"
if grep -Eiq 'PANIC|panicked at|fatal runtime error|stack overflow' "$CANARY_DIR/server.log"; then
  echo "[canary] panic marker found in server log" >&2
  exit 69
fi

if ! kill -TERM "$SERVER_PID" 2>/dev/null; then
  echo "[canary] could not request graceful shutdown" >&2
  exit 70
fi
stopped=0
if wait_for_pid_exit "$SERVER_PID" 100; then
  stopped=1
fi
if [ "$stopped" -ne 1 ]; then
  echo "[canary] graceful shutdown exceeded 10 seconds" >&2
  exit 70
fi
set +e
wait "$SERVER_PID"
SERVER_STATUS=$?
set -e
SERVER_PID=
if [ "$SERVER_STATUS" -ne 0 ]; then
  echo "[canary] graceful server shutdown returned $SERVER_STATUS" >&2
  exit 70
fi

{
  echo "game_port=$GAME_PORT"
  echo "metrics_port=$METRICS_PORT"
  echo "players=$PLAYERS"
  echo "seconds=$SECONDS_TO_RUN"
  echo "hard_timeout_seconds=$CANARY_HARD_TIMEOUT_SECONDS"
  echo "pulse_before=$PULSE_BEFORE"
  echo "pulse_after=$PULSE_AFTER"
} >"$ARTIFACTS/manifest.txt"
echo "[canary] GREEN: artifacts in $ARTIFACTS"
