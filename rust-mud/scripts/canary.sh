#!/usr/bin/env bash
# Isolated, fail-closed DeltaMUD live canary runner.
set -Eeuo pipefail

EVIDENCE_STAGE_SOURCE=
EVIDENCE_STAGE_DEST=
if [ "${1:-}" = --stage-evidence ]; then
  [ "$#" -eq 3 ] || {
    echo "usage: $0 --stage-evidence SOURCE DEST" >&2
    exit 64
  }
  EVIDENCE_STAGE_SOURCE=$2
  EVIDENCE_STAGE_DEST=$3
  shift 3
fi

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

CURRENT_UID=$(id -u)
if ! awk -v expected="$CURRENT_UID" '
  /^Uid:/ {
    uid_seen = 1
    if ($2 != expected || $3 != expected || $4 != expected || $5 != expected) bad = 1
  }
  /^Cap(Inh|Prm|Eff|Amb):/ { caps_seen++; if ($2 !~ /^0+$/) bad = 1 }
  END { exit !(uid_seen && caps_seen == 4 && !bad && expected != 0) }
' "/proc/$$/status"; then
  echo "[canary] refusing root, set-ID, or capability-bearing execution" >&2
  exit 77
fi

copy_evidence_file () {
  /usr/bin/python3 -I - "$1" "$2" "$CURRENT_UID" "$3" "$4" <<'PY'
import os
import stat
import sys

source, destination = sys.argv[1:3]
expected_uid, expected_device, max_bytes = map(int, sys.argv[3:6])
required_flags = ("O_CLOEXEC", "O_NOFOLLOW")
if any(not hasattr(os, name) for name in required_flags):
    raise SystemExit("required no-follow file APIs are unavailable")

source_fd = os.open(source, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
destination_fd = -1
try:
    before = os.fstat(source_fd)
    if not stat.S_ISREG(before.st_mode):
        raise RuntimeError("source is not a regular file")
    if before.st_uid != expected_uid or before.st_nlink != 1:
        raise RuntimeError("source ownership or link count changed")
    if before.st_dev != expected_device or not 0 <= before.st_size <= max_bytes:
        raise RuntimeError("source device or size changed")

    destination_fd = os.open(
        destination,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
        0o600,
    )
    copied = 0
    while True:
        chunk = os.read(source_fd, 1024 * 1024)
        if not chunk:
            break
        copied += len(chunk)
        if copied > before.st_size or copied > max_bytes:
            raise RuntimeError("source grew while being copied")
        view = memoryview(chunk)
        while view:
            written = os.write(destination_fd, view)
            if written <= 0:
                raise RuntimeError("short destination write")
            view = view[written:]
    if copied != before.st_size:
        raise RuntimeError("source size changed while being copied")
    os.fsync(destination_fd)

    after = os.fstat(source_fd)
    stable_fields = (
        "st_dev", "st_ino", "st_mode", "st_uid", "st_gid", "st_nlink",
        "st_size", "st_mtime_ns", "st_ctime_ns",
    )
    if any(getattr(before, field) != getattr(after, field) for field in stable_fields):
        raise RuntimeError("source metadata changed while being copied")
    staged = os.fstat(destination_fd)
    if (
        not stat.S_ISREG(staged.st_mode)
        or staged.st_uid != expected_uid
        or staged.st_nlink != 1
        or staged.st_size != copied
    ):
        raise RuntimeError("staged destination validation failed")
finally:
    if destination_fd >= 0:
        os.close(destination_fd)
    os.close(source_fd)

directory_fd = os.open(
    os.path.dirname(destination),
    os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
)
try:
    os.fsync(directory_fd)
finally:
    os.close(directory_fd)
PY
}

stage_evidence () {
  local source=$1
  local destination=$2
  local canonical_source canonical_destination destination_parent
  local source_device entry relative target
  local metadata mode_hex owner mode links device inode size
  local final_inventory final_source_inventory
  local entry_type file_count=0 directory_count=0 total_bytes=0
  local rejected= reason= inventory=
  local max_files=128 max_directories=64 max_bytes=$((64 * 1024 * 1024))

  case "$source:$destination" in
    /*:/*) ;;
    *) echo "[canary-evidence] source and destination must be absolute paths" >&2; return 65 ;;
  esac
  [ "$source" != "$destination" ] || {
    echo "[canary-evidence] source and destination must differ" >&2
    return 65
  }
  canonical_source=$(realpath -m -- "$source") || return 65
  canonical_destination=$(realpath -m -- "$destination") || return 65
  case "$canonical_destination/" in
    "$canonical_source/"*)
      echo "[canary-evidence] destination must not be inside the raw source" >&2
      return 65
      ;;
  esac
  if [ -e "$destination" ] || [ -L "$destination" ]; then
    echo "[canary-evidence] destination already exists: $destination" >&2
    return 65
  fi
  destination_parent=${destination%/*}
  [ -n "$destination_parent" ] || destination_parent=/
  [ -d "$destination_parent" ] && [ ! -L "$destination_parent" ] || {
    echo "[canary-evidence] destination parent must be a real directory" >&2
    return 65
  }

  umask 077
  mkdir -m 0700 -- "$destination"
  inventory="$destination/.source-inventory"

  if [ ! -d "$source" ] || [ -L "$source" ]; then
    rejected=1
    reason="source is missing or is not a real directory"
  elif ! metadata=$(stat -c '%f:%u:%a:%h:%d:%i:%s' -- "$source"); then
    rejected=1
    reason="source metadata is unreadable"
  else
    IFS=: read -r mode_hex owner mode links source_device inode size <<<"$metadata"
    if [ "$owner" != "$CURRENT_UID" ] \
      || [ "$((16#$mode_hex & 16#f000))" -ne "$((16#4000))" ] \
      || [ "$((8#$mode & 8#022))" -ne 0 ]; then
      rejected=1
      reason="source directory ownership, type, or permissions are unsafe"
    elif ! find -P "$source" -xdev -print0 | LC_ALL=C sort -z >"$inventory"; then
      rejected=1
      reason="source inventory could not be read completely"
    fi
  fi

  if [ -z "$rejected" ]; then
    while IFS= read -r -d '' entry; do
      if ! metadata=$(stat -c '%f:%u:%a:%h:%d:%i:%s' -- "$entry"); then
        rejected=1
        reason="an evidence entry became unreadable"
        break
      fi
      IFS=: read -r mode_hex owner mode links device inode size <<<"$metadata"
      entry_type=$((16#$mode_hex & 16#f000))
      if [ "$owner" != "$CURRENT_UID" ] || [ "$device" != "$source_device" ]; then
        rejected=1
        reason="an evidence entry has a foreign owner or crosses a filesystem boundary"
        break
      fi
      case "$entry_type" in
        $((16#4000)))
          directory_count=$((directory_count + 1))
          if [ "$((8#$mode & 8#022))" -ne 0 ] \
            || [ "$directory_count" -gt "$max_directories" ]; then
            rejected=1
            reason="an evidence directory is writable by another identity or the tree is too deep"
            break
          fi
          ;;
        $((16#8000)))
          file_count=$((file_count + 1))
          if ! [[ "$size" =~ ^[0-9]+$ ]] || [ "$size" -gt "$max_bytes" ] \
            || [ "$total_bytes" -gt "$((max_bytes - size))" ] \
            || [ "$links" -ne 1 ] || [ "$((8#$mode & 8#022))" -ne 0 ] \
            || [ "$file_count" -gt "$max_files" ]; then
            rejected=1
            reason="an evidence file is linked, writable by another identity, or exceeds staging limits"
            break
          fi
          total_bytes=$((total_bytes + size))
          ;;
        *)
          rejected=1
          reason="the evidence tree contains a link or special node"
          break
          ;;
      esac
    done <"$inventory"
  fi

  if [ -n "$rejected" ]; then
    rm -f -- "$inventory"
    printf '%s\n' \
      "Canary evidence was rejected at the safe upload boundary." \
      "Reason: $reason" \
      "No files from the raw evidence tree were staged or uploaded." \
      >"$destination/EVIDENCE_REJECTED.txt"
  else
    while IFS= read -r -d '' entry; do
      [ "$entry" != "$source" ] || continue
      relative=${entry#"$source"/}
      metadata=$(stat -c '%f:%u:%a:%h:%d:%i:%s' -- "$entry") || return 72
      IFS=: read -r mode_hex owner mode links device inode size <<<"$metadata"
      entry_type=$((16#$mode_hex & 16#f000))
      [ "$owner" = "$CURRENT_UID" ] && [ "$device" = "$source_device" ] || return 72
      case "$entry_type" in
        $((16#4000)))
          [ "$((8#$mode & 8#022))" -eq 0 ] || return 72
          mkdir -m 0700 -- "$destination/$relative"
          ;;
        $((16#8000))) [ "$links" -eq 1 ] || return 72 ;;
        *) return 72 ;;
      esac
    done <"$inventory"

    while IFS= read -r -d '' entry; do
      [ "$entry" != "$source" ] || continue
      relative=${entry#"$source"/}
      metadata=$(stat -c '%f:%u:%a:%h:%d:%i:%s' -- "$entry") || return 72
      IFS=: read -r mode_hex owner mode links device inode size <<<"$metadata"
      entry_type=$((16#$mode_hex & 16#f000))
      [ "$entry_type" -eq "$((16#4000))" ] && continue
      [ "$entry_type" -eq "$((16#8000))" ] || return 72
      [ "$owner" = "$CURRENT_UID" ] && [ "$links" -eq 1 ] \
        && [ "$device" = "$source_device" ] || return 72
      target="$destination/$relative"
      copy_evidence_file "$entry" "$target" "$source_device" "$max_bytes" \
        || return 72
    done <"$inventory"

    final_source_inventory="$destination/.source-inventory-after"
    if ! find -P "$source" -xdev -print0 \
      | LC_ALL=C sort -z >"$final_source_inventory" \
      || ! cmp -s -- "$inventory" "$final_source_inventory"; then
      echo "[canary-evidence] source inventory changed while it was staged" >&2
      return 72
    fi
    rm -f -- "$final_source_inventory"
    rm -f -- "$inventory"
  fi

  final_inventory=$(mktemp "$destination_parent/.canary-upload-inventory.XXXXXX") \
    || return 72
  if ! find -P "$destination" -xdev -depth -print0 >"$final_inventory"; then
    rm -f -- "$final_inventory"
    echo "[canary-evidence] final upload tree could not be inventoried" >&2
    return 72
  fi
  while IFS= read -r -d '' entry; do
    metadata=$(stat -c '%f:%u:%a:%h:%d:%i:%s' -- "$entry") || return 72
    IFS=: read -r mode_hex owner mode links device inode size <<<"$metadata"
    entry_type=$((16#$mode_hex & 16#f000))
    [ "$owner" = "$CURRENT_UID" ] || return 72
    case "$entry_type" in
      $((16#4000))) chmod 0500 -- "$entry" || return 72 ;;
      $((16#8000)))
        [ "$links" -eq 1 ] || return 72
        chmod 0400 -- "$entry" || return 72
        ;;
      *) echo "[canary-evidence] unsafe entry reached the upload tree" >&2; return 72 ;;
    esac
  done <"$final_inventory"
  rm -f -- "$final_inventory"
  echo "[canary-evidence] safe upload tree staged at $destination"
}

if [ -n "$EVIDENCE_STAGE_SOURCE" ]; then
  stage_evidence "$EVIDENCE_STAGE_SOURCE" "$EVIDENCE_STAGE_DEST"
  exit $?
fi

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
if [ "${DELTAMUD_CANARY_SANDBOXED:-0}" != 1 ] \
  && [ "${DELTAMUD_CANARY_TIMEOUT_PARENT:-}" != "$PPID" ]; then
  CANARY_CHILD_ENV=(
    env -i PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
    HOME=/var/tmp TMPDIR=/var/tmp LANG=C.UTF-8 LC_ALL=C.UTF-8 TZ=UTC
    "DELTAMUD_CANARY_TIMEOUT_PARENT=$$"
    "CANARY_HARD_TIMEOUT_SECONDS=$CANARY_HARD_TIMEOUT_SECONDS"
    "CANARY_KILL_AFTER_SECONDS=$CANARY_KILL_AFTER_SECONDS"
  )
  [ -z "${RUST_BIN:-}" ] || CANARY_CHILD_ENV+=("RUST_BIN=$RUST_BIN")
  [ -z "${MUD_CANARY_SOURCE_LIB:-}" ] \
    || CANARY_CHILD_ENV+=("MUD_CANARY_SOURCE_LIB=$MUD_CANARY_SOURCE_LIB")
  unset LD_AUDIT LD_DEBUG LD_LIBRARY_PATH LD_PRELOAD PYTHONHOME PYTHONPATH
  exec "${CANARY_CHILD_ENV[@]}" \
    timeout --signal=TERM --kill-after="$CANARY_KILL_AFTER_SECONDS" \
    "$CANARY_HARD_TIMEOUT_SECONDS" bash "$0" "${CANARY_ARGS[@]}"
fi
unset DELTAMUD_CANARY_TIMEOUT_PARENT

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
MUD_DIR=$(cd "$SCRIPT_DIR/.." && pwd)
REPO_DIR=$(cd "$MUD_DIR/.." && pwd)
BINARY=${RUST_BIN:-$MUD_DIR/target/release/deltamud}
SOURCE_LIB=${MUD_CANARY_SOURCE_LIB:-$REPO_DIR/lib}
if [ ! -x "$BINARY" ]; then
  echo "[canary] release binary missing: $BINARY" >&2
  exit 65
fi
case "$SOURCE_LIB" in
  /*) ;;
  *) echo "[canary] source lib path must be absolute: $SOURCE_LIB" >&2; exit 65 ;;
esac
if [ ! -d "$SOURCE_LIB" ] || [ -L "$SOURCE_LIB" ]; then
  echo "[canary] source lib must be a real directory: $SOURCE_LIB" >&2
  exit 65
fi
if ! SOURCE_SPECIAL=$(find "$SOURCE_LIB" \! -type d \! -type f -print -quit); then
  echo "[canary] could not verify the source lib tree" >&2
  exit 65
fi
if [ -n "$SOURCE_SPECIAL" ]; then
  echo "[canary] source lib contains a link or special file: $SOURCE_SPECIAL" >&2
  exit 65
fi

if [ "${DELTAMUD_CANARY_SANDBOXED:-0}" = 1 ]; then
  [ "$ARTIFACTS" = /artifacts ] || {
    echo "[canary] sandbox artifact mount is not canonical" >&2
    exit 77
  }
elif [ -z "$ARTIFACTS" ]; then
  ARTIFACTS=$(mktemp -d /var/tmp/deltamud-canary-artifacts.XXXXXX)
else
  if [ -e "$ARTIFACTS" ] || [ -L "$ARTIFACTS" ]; then
    echo "[canary] artifact path already exists; refusing to mix or overwrite evidence: $ARTIFACTS" >&2
    exit 65
  fi
  umask 077
  mkdir -m 0700 -- "$ARTIFACTS"
fi
[ -d "$ARTIFACTS" ] && [ ! -L "$ARTIFACTS" ] \
  && [ "$(stat -Lc %u:%a -- "$ARTIFACTS")" = "$(id -u):700" ] || {
    echo "[canary] artifact directory must be private, real, and invoking-user-owned" >&2
    exit 65
  }
if [ "${DELTAMUD_CANARY_SANDBOXED:-0}" != 1 ]; then
  echo "[canary] evidence directory: $ARTIFACTS"
fi

tree_digest () {
  local root=$1
  (
    cd -- "$root"
    find . -type f -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum --
  ) | sha256sum | awk '{print $1}'
}

if [ "${DELTAMUD_CANARY_SANDBOXED:-0}" != 1 ]; then
  [ -f /usr/bin/bwrap ] && [ ! -L /usr/bin/bwrap ] \
    && [ -x /usr/bin/bwrap ] && [ "$(stat -Lc %u -- /usr/bin/bwrap)" -eq 0 ] \
    && [ "$((8#$(stat -Lc %a -- /usr/bin/bwrap) & 8#022))" -eq 0 ] || {
    echo "[canary] a root-owned, non-writable /usr/bin/bwrap is required" >&2
    exit 77
  }
  CANARY_STAGE=$(mktemp -d /var/tmp/deltamud-canary-stage.XXXXXX)
  cleanup_canary_stage () {
    local stage_rc=$?
    trap - EXIT INT TERM
    chmod -R u+rwX "$CANARY_STAGE" 2>/dev/null || true
    rm -rf -- "$CANARY_STAGE" || [ "$stage_rc" -ne 0 ] || stage_rc=73
    exit "$stage_rc"
  }
  trap cleanup_canary_stage EXIT INT TERM
  install -d -m 0755 "$CANARY_STAGE/input/bin" "$CANARY_STAGE/input/scripts" \
    "$CANARY_STAGE/input/lib"
  install -d -m 0700 "$CANARY_STAGE/work" "$CANARY_STAGE/work/tmp"
  BINARY_HASH=$(sha256sum -- "$BINARY" | awk '{print $1}')
  SOURCE_DIGEST=$(tree_digest "$SOURCE_LIB")
  install -m 0755 "$BINARY" "$CANARY_STAGE/input/bin/deltamud"
  install -m 0555 "$SCRIPT_DIR/canary.sh" "$SCRIPT_DIR/soak_combat.py" \
    "$CANARY_STAGE/input/scripts/"
  cp -R --no-preserve=ownership,mode,timestamps -- \
    "$SOURCE_LIB/." "$CANARY_STAGE/input/lib/"
  [ "$BINARY_HASH" = "$(sha256sum -- "$CANARY_STAGE/input/bin/deltamud" | awk '{print $1}')" ] \
    && [ "$SOURCE_DIGEST" = "$(tree_digest "$CANARY_STAGE/input/lib")" ] || {
      echo "[canary] staged binary or lib did not match its source" >&2
      exit 65
    }
  if ! STAGE_SPECIAL=$(find "$CANARY_STAGE/input" \! -type d \! -type f -print -quit); then
    echo "[canary] could not verify staged sandbox inputs" >&2
    exit 65
  fi
  [ -z "$STAGE_SPECIAL" ] || {
    echo "[canary] staged sandbox input contains a link or special file: $STAGE_SPECIAL" >&2
    exit 65
  }
  chmod -R a-w "$CANARY_STAGE/input"
  CANARY_HOST_NETNS=$(readlink /proc/self/ns/net)
  SANDBOX_ARGS=(--seconds "$SECONDS_TO_RUN" --players "$PLAYERS" --artifacts /artifacts)
  [ -z "$NEGATIVE_CONTROL" ] \
    || SANDBOX_ARGS+=(--negative-control "$NEGATIVE_CONTROL")
  set +e
  env -i PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    HOME=/var/tmp TMPDIR=/var/tmp LANG=C.UTF-8 LC_ALL=C.UTF-8 TZ=UTC \
    /usr/bin/bwrap --unshare-all --new-session --die-with-parent --uid 1 --gid 1 \
    --cap-drop ALL --proc /proc --dev /dev --tmpfs /run \
    --ro-bind /usr /usr --symlink usr/bin /bin --symlink usr/sbin /sbin \
    --symlink usr/lib /lib --symlink usr/lib64 /lib64 --dir /etc \
    --ro-bind /etc/alternatives /etc/alternatives \
    --ro-bind /etc/ld.so.cache /etc/ld.so.cache \
    --ro-bind /etc/passwd /etc/passwd --ro-bind /etc/group /etc/group \
    --ro-bind /etc/nsswitch.conf /etc/nsswitch.conf \
    --ro-bind /etc/hosts /etc/hosts --ro-bind /etc/resolv.conf /etc/resolv.conf \
    --dir /var --dir /var/tmp --ro-bind "$CANARY_STAGE/input" /input \
    --bind "$CANARY_STAGE/work" /work --bind "$CANARY_STAGE/work/tmp" /tmp \
    --bind "$ARTIFACTS" /artifacts --chdir /work --clearenv \
    --setenv PATH /usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    --setenv HOME /work --setenv TMPDIR /work/tmp --setenv LANG C.UTF-8 \
    --setenv LC_ALL C.UTF-8 --setenv TZ UTC --setenv PYTHONDONTWRITEBYTECODE 1 \
    --setenv DELTAMUD_CANARY_SANDBOXED 1 \
    --setenv CANARY_HOST_NETNS "$CANARY_HOST_NETNS" \
    --setenv CANARY_HARD_TIMEOUT_SECONDS "$CANARY_HARD_TIMEOUT_SECONDS" \
    --setenv CANARY_KILL_AFTER_SECONDS "$CANARY_KILL_AFTER_SECONDS" \
    --setenv RUST_BIN /input/bin/deltamud \
    --setenv MUD_CANARY_SOURCE_LIB /input/lib \
    /bin/bash /input/scripts/canary.sh "${SANDBOX_ARGS[@]}"
  SANDBOX_RC=$?
  set -e
  [ "$BINARY_HASH" = "$(sha256sum -- "$BINARY" | awk '{print $1}')" ] \
    && [ "$BINARY_HASH" = "$(sha256sum -- "$CANARY_STAGE/input/bin/deltamud" | awk '{print $1}')" ] \
    && [ "$SOURCE_DIGEST" = "$(tree_digest "$SOURCE_LIB")" ] || {
      echo "[canary] source or staged binary/lib changed during the sandboxed run" >&2
      case "$SANDBOX_RC" in
        0|91|92|93) SANDBOX_RC=65 ;;
      esac
    }
  ARTIFACT_UNSAFE=$(find "$ARTIFACTS" -xdev \
    \( \! -type d -a \! -type f -o \! -user "$(id -u)" -o -type f -links +1 \) \
    -print -quit 2>/dev/null || printf '%s' unreadable)
  if [ -n "$ARTIFACT_UNSAFE" ] || [ ! -d "$ARTIFACTS" ] || [ -L "$ARTIFACTS" ] \
    || [ "$(stat -Lc %u:%a -- "$ARTIFACTS" 2>/dev/null || true)" \
      != "$(id -u):700" ]; then
    echo "[canary] artifact tree contains a link, special node, hardlink, foreign owner, or unreadable entry: $ARTIFACT_UNSAFE" >&2
    case "$SANDBOX_RC" in
      0|91|92|93) SANDBOX_RC=65 ;;
    esac
  fi
  if ! chmod -R u+rwX "$CANARY_STAGE" 2>/dev/null \
    || ! rm -rf -- "$CANARY_STAGE"; then
    case "$SANDBOX_RC" in
      0|91|92|93) SANDBOX_RC=73 ;;
    esac
  fi
  CANARY_STAGE=
  trap - EXIT INT TERM
  [ "$SANDBOX_RC" -eq 0 ] \
    && echo "[canary] GREEN: artifacts in $ARTIFACTS"
  exit "$SANDBOX_RC"
fi

[ "$(id -u)" -eq 1 ] || {
  echo "[canary] unexpected sandbox identity" >&2
  exit 77
}
awk '
  /^Uid:/ { seen_uid = 1; if ($2 != 1 || $3 != 1 || $4 != 1 || $5 != 1) bad = 1 }
  /^Cap(Inh|Prm|Eff|Bnd|Amb):/ { seen_caps++; if ($2 !~ /^0+$/) bad = 1 }
  END { exit !(seen_uid && seen_caps == 5 && !bad) }
' /proc/$$/status || {
  echo "[canary] sandbox retained an identity or capability" >&2
  exit 77
}
[ -n "${CANARY_HOST_NETNS:-}" ] \
  && [ "$(readlink /proc/self/ns/net)" != "$CANARY_HOST_NETNS" ] || {
    echo "[canary] private network namespace proof failed" >&2
    exit 77
  }
ip -o link show lo | grep -Eq '<[^>]*UP' || {
  echo "[canary] private loopback is not up" >&2
  exit 77
}
for protected_target in /usr /input; do
  awk -v target="$protected_target" '
    $5 == target && ("," $6 ",") ~ /,ro,/ { found = 1 }
    END { exit found ? 0 : 1 }
  ' /proc/self/mountinfo || {
    echo "[canary] sandbox input is not read-only" >&2
    exit 77
  }
done
verify_only_private_mounts_are_writable () {
  local mount_id parent_id device mount_root mount_target mount_options remainder
  local records=0
  while IFS=' ' read -r mount_id parent_id device mount_root mount_target \
    mount_options remainder; do
    records=$((records + 1))
    case ",$mount_options," in
      *,rw,*)
        case "$mount_target" in
          /|/artifacts|/work|/tmp|/run|/dev|/dev/*|/proc|/proc/*) ;;
          *) echo "[canary] unexpected writable mount: $mount_target" >&2; return 1 ;;
        esac
        ;;
    esac
  done </proc/self/mountinfo
  [ "$records" -gt 0 ]
}
verify_only_private_mounts_are_writable || {
  echo "[canary] a non-private mount remains writable in the sandbox" >&2
  exit 77
}
if find /run /tmp /dev /work -type s -print -quit | grep -q .; then
  echo "[canary] sandbox inherited a host Unix-domain socket" >&2
  exit 77
fi

CANARY_DIR=$(mktemp -d /work/deltamud-canary.XXXXXX)
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
      cleanup_rc=71
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
  if [ "$cleanup_rc" -ne 0 ]; then
    case "$rc" in
      0|91|92|93) rc=$cleanup_rc ;;
    esac
  fi
  exit "$rc"
}
trap cleanup EXIT INT TERM

mkdir -p "$CANARY_DIR/lib"
cp -R --no-preserve=ownership,mode,timestamps -- "$SOURCE_LIB/." "$CANARY_DIR/lib/"
chmod -R u+rwX "$CANARY_DIR/lib"
if ! CANARY_SPECIAL=$(find "$CANARY_DIR/lib" \! -type d \! -type f -print -quit); then
  echo "[canary] could not verify the private lib copy" >&2
  exit 65
fi
if [ -n "$CANARY_SPECIAL" ]; then
  echo "[canary] private lib copy contains a link or special file: $CANARY_SPECIAL" >&2
  exit 65
fi
rm -rf -- "$CANARY_DIR/lib/plrobjs" "$CANARY_DIR/lib/plralias"
rm -f -- "$CANARY_DIR/lib/etc/date_record" "$CANARY_DIR/lib/USRCNT"
mkdir -p "$CANARY_DIR/lib/plrobjs" "$CANARY_DIR/lib/plralias"

choose_port () {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()'
}
GAME_PORT=$(choose_port)
METRICS_PORT=$(choose_port)
while [ "$METRICS_PORT" -eq "$GAME_PORT" ]; do METRICS_PORT=$(choose_port); done

env -i PATH=/usr/local/bin:/usr/bin:/bin HOME="$CANARY_DIR" \
  TMPDIR="$CANARY_DIR" LANG=C.UTF-8 LC_ALL=C.UTF-8 TZ=UTC \
  MUD_MOCK_DB=true MUD_BIND=127.0.0.1 MUD_PORT="$GAME_PORT" \
  MUD_METRICS_BIND=127.0.0.1 MUD_METRICS_PORT="$METRICS_PORT" \
  MUD_LIB_PATH="$CANARY_DIR/lib" MUD_EXEC_PATH="$BINARY" \
  MUD_RNG_SEED=424242 MUD_DB_TIMEOUT_SECS=5 \
  MUD_REVERSE_DNS=false MUD_REVERSE_DNS_TIMEOUT_MS=1000 \
  MUD_REVERSE_DNS_MAX_INFLIGHT=16 MUD_WWW_WHO=0 \
  MUD_WWW_WHO_DIR="$CANARY_DIR/www" MUD_AUTOREBOOT=0 \
  MUD_PT_MARKABLE=0 MUD_COMPAT_MODE=0 MUD_CFORMAT_FILES=0 \
  MUD_NO_SPECIALS=0 MUD_ENFORCE_MULTIPLAY=0 MUD_MAX_CONN=128 \
  MUD_CONN_BURST=32 MUD_CONN_WINDOW_MS=1000 \
  "$BINARY" >"$CANARY_DIR/server.log" 2>&1 &
SERVER_PID=$!
CURL=(curl --disable --noproxy '*' --proto '=http' --fail --silent --max-time 2)

ready=0
for _ in $(seq 1 200); do
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "[canary] server exited during boot" >&2
    exit 66
  fi
  if "${CURL[@]}" "http://127.0.0.1:$METRICS_PORT/ready" >/dev/null; then
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
    echo "[canary-negative] observed kill-server" >&2
    exit 91
    ;;
  freeze-pulses)
    kill -STOP "$SERVER_PID"
    SERVER_STOPPED=1
    sleep 1
    if "${CURL[@]}" "http://127.0.0.1:$METRICS_PORT/ready" >/dev/null; then
      echo "[canary] frozen server unexpectedly answered readiness" >&2
      exit 76
    fi
    echo "[canary-negative] observed freeze-pulses" >&2
    exit 92
    ;;
  driver) ;;
  *) echo "[canary] unknown negative control: $NEGATIVE_CONTROL" >&2; exit 64 ;;
esac

"${CURL[@]}" "http://127.0.0.1:$METRICS_PORT/metrics" >"$CANARY_DIR/metrics.before"
PULSE_BEFORE=$(awk '$1 == "deltamud_pulse" {print $2}' "$CANARY_DIR/metrics.before")
sleep 1
"${CURL[@]}" "http://127.0.0.1:$METRICS_PORT/metrics" >"$CANARY_DIR/metrics.after"
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
if [ "$NEGATIVE_CONTROL" = driver ]; then
  set +e
  timeout --foreground --signal=TERM --kill-after=5 "$((SECONDS_TO_RUN + 90))" \
    python3 "$SCRIPT_DIR/soak_combat.py" \
      --port "$GAME_PORT" --readiness "$METRICS_PORT" --players "$PLAYERS" \
      --seconds "$SECONDS_TO_RUN" --log "$CANARY_DIR/server.log" \
      --artifacts "$ARTIFACTS" "${DRIVER_EXTRA[@]}" \
      2>&1 | tee "$ARTIFACTS/driver-negative.log"
  DRIVER_STATUS=${PIPESTATUS[0]}
  set -e
  if [ "$DRIVER_STATUS" -ne 1 ] \
    || ! grep -Fq '[soak] RED: injected driver failure' \
      "$ARTIFACTS/driver-negative.log"; then
    echo "[canary] driver negative control failed for an unexpected reason (status $DRIVER_STATUS)" >&2
    exit 76
  fi
  echo "[canary-negative] observed driver" >&2
  exit 93
else
  timeout --foreground --signal=TERM --kill-after=5 "$((SECONDS_TO_RUN + 90))" \
    python3 "$SCRIPT_DIR/soak_combat.py" \
      --port "$GAME_PORT" \
      --readiness "$METRICS_PORT" \
      --players "$PLAYERS" \
      --seconds "$SECONDS_TO_RUN" \
      --log "$CANARY_DIR/server.log" \
      --artifacts "$ARTIFACTS"
fi

if ! kill -0 "$SERVER_PID" 2>/dev/null; then
  echo "[canary] server exited during the live workload" >&2
  exit 69
fi
"${CURL[@]}" "http://127.0.0.1:$METRICS_PORT/ready" >"$ARTIFACTS/ready.after"
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

for required in "$CANARY_DIR/server.log" "$CANARY_DIR/metrics.before" \
  "$CANARY_DIR/metrics.after" "$ARTIFACTS/ready.after"; do
  [ -f "$required" ] && [ ! -L "$required" ] && [ -s "$required" ] || {
    echo "[canary] required evidence is missing, linked, or empty: $required" >&2
    exit 72
  }
done
TRANSCRIPT_COUNT=$(find "$ARTIFACTS" -maxdepth 1 -type f \
  -name 'Soak*.transcript.txt' -size +0c -printf . | wc -c)
[ "$TRANSCRIPT_COUNT" -eq "$PLAYERS" ] || {
  echo "[canary] expected $PLAYERS nonempty player transcripts, found $TRANSCRIPT_COUNT" >&2
  exit 72
}
set +e
grep -Eiq 'PANIC|panicked at|fatal runtime error|stack overflow' "$CANARY_DIR/server.log"
POST_SHUTDOWN_PANIC_STATUS=$?
set -e
case "$POST_SHUTDOWN_PANIC_STATUS" in
  0) echo "[canary] panic marker found during shutdown" >&2; exit 69 ;;
  1) ;;
  *) echo "[canary] could not scan the final server log" >&2; exit 72 ;;
esac
is_expected_sandbox_supervisor () {
  local proc_dir=$1
  local argv0=
  IFS= read -r -d '' argv0 <"$proc_dir/cmdline" 2>/dev/null || return 1
  [ "${proc_dir##*/}" = 1 ] \
    && [ "$(readlink -e "$proc_dir/exe" 2>/dev/null || true)" = /usr/bin/bwrap ] \
    && [ "$argv0" = /usr/bin/bwrap ] \
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
    ' "$proc_dir/status"
}

for proc_dir in /proc/[0-9]*; do
  [ "${proc_dir##*/}" = "$$" ] && continue
  is_expected_sandbox_supervisor "$proc_dir" && continue
  echo "[canary] residual PID ${proc_dir##*/} remained after server shutdown" >&2
  exit 71
done

{
  echo "game_port=$GAME_PORT"
  echo "metrics_port=$METRICS_PORT"
  echo "players=$PLAYERS"
  echo "seconds=$SECONDS_TO_RUN"
  echo "hard_timeout_seconds=$CANARY_HARD_TIMEOUT_SECONDS"
  echo "pulse_before=$PULSE_BEFORE"
  echo "pulse_after=$PULSE_AFTER"
} >"$ARTIFACTS/manifest.txt"
echo "[canary] live checks passed; finalizing evidence in $ARTIFACTS"
