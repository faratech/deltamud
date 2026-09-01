#!/bin/bash
# Build, canary, install, and activate one immutable DeltaMUD Rust release.
set -Eeuo pipefail
PATH=/usr/sbin:/usr/bin:/sbin:/bin
IFS=$' \t\n'
umask 022

TRUSTED_MANAGER=/usr/local/sbin/deltamud-release
REPO_DIR=/web/deltamud
MUD_DIR=$REPO_DIR/rust-mud
SCRIPT_DIR=$MUD_DIR/scripts
TOOLCHAIN_ROOT=/opt/deltamud/toolchains/1.98.0
CARGO_AUDIT_BIN=$TOOLCHAIN_ROOT/bin/cargo-audit
CARGO_AUDIT_VERSION=0.22.2
RELEASE_ROOT=/opt/deltamud/releases
CURRENT_LINK=/opt/deltamud/current
PREVIOUS_LINK=/opt/deltamud/previous
SERVICE=deltamud.service
READY_URL=http://127.0.0.1:19595/ready
BUILD_USER=
BUILD_UID=
BUILD_GROUP=
BUILD_WORK=
SOURCE_ROOT=
SOURCE_MUD=
SOURCE_SNAPSHOT_SHA256=
DEPLOY_STAGE=
CANARY_ARTIFACT_PARENT=
ACTIVATION_TEMP_LINKS=()
ACTIVATION_PENDING=0
ACTIVATION_TARGET=
ACTIVATION_OLD=
ACTIVATION_HAD_CURRENT=0
ACTIVATION_PRIOR_PREVIOUS=
ACTIVATION_HAD_PREVIOUS=0
ACTIVATION_RECOVERY_MANIFEST=none
ACTIVATION_CONTENT_MANIFEST=none
APPROVAL_TEMP=
CONTENT_APPROVAL=/etc/deltamud/content-approved
CONTENT_APPROVAL_MANIFEST=none
ACTIVATION_ROOT=/etc/deltamud/activation
ACTIVATION_MARKER=$ACTIVATION_ROOT/pending
ACTIVATION_MARKER_TEMP=
MAIN_UNIT_SHA256=5375a4949ecdd901981a8cb471a3ed03beb872d4add824de363b04d110db4832
MIGRATION_UNIT_SHA256=702e9b34fc0d523e8f23684ff1ec34ebbdff30849e1e264f4b8991a674b9f6ca
BACKUP_ROOT=/var/backups/deltamud
BACKUP_CNF=/etc/deltamud/backup.cnf
DATABASE_ENV=/etc/deltamud/deltamud.env
BACKUP_STAGE=
BACKUP_MANIFEST=
RESTORE_CHECK_SCHEMA=
DATABASE_IDENTITY_HASH=
DATABASE_ENV_HASH=
BACKUP_ENDPOINT_HOST=
BACKUP_ENDPOINT_PORT=
DEFERRED_SIGNAL=0
RUNTIME_ENV_ARGS=()

usage () {
  echo "usage: $0 deploy <git-sha> --acknowledge-no-state-restore | install <git-sha> | backup <installed-git-sha> | initialize-backup <installed-git-sha> --acknowledge-empty-database | content-approve <installed-git-sha> <backup-manifest> --acknowledge-reviewed-runtime-merge | activate <installed-git-sha> --acknowledge-no-state-restore | migrate-activate <installed-git-sha> [--acknowledge-reconciled-state] | initialize-migrate-activate <installed-git-sha> --acknowledge-empty-database | rollback <installed-git-sha> --acknowledge-no-state-restore | bootstrap-implementor <installed-git-sha> <player-name> --acknowledge-offline-authority-bootstrap | activation-recover | activation-resolve <installed-git-sha> --acknowledge-reconciled-state" >&2
  exit 64
}

require_trusted_manager () {
  local invoked_as manager_mode
  invoked_as=$(readlink -f -- "${BASH_SOURCE[0]}") || {
    echo "could not resolve release-manager path" >&2
    exit 77
  }
  [ "$invoked_as" = "$TRUSTED_MANAGER" ] || {
    echo "refusing privileged execution from the writable checkout" >&2
    echo "install the reviewed manager at $TRUSTED_MANAGER (root:root mode 0755)" >&2
    exit 77
  }
  [ -f "$TRUSTED_MANAGER" ] && [ ! -L "$TRUSTED_MANAGER" ] \
    && [ "$(stat -Lc %u -- "$TRUSTED_MANAGER")" -eq 0 ] || {
      echo "trusted release manager must be a root-owned regular file" >&2
      exit 77
    }
  manager_mode=$(stat -Lc %a -- "$TRUSTED_MANAGER")
  [ "$((8#$manager_mode & 8#022))" -eq 0 ] || {
    echo "trusted release manager must not be group/world writable" >&2
    exit 77
  }
  validate_root_directory /usr || exit 77
  validate_root_directory /usr/local || exit 77
  validate_root_directory /usr/local/sbin || exit 77
}

validate_sha () {
  [[ "$1" =~ ^[0-9a-f]{40}$ ]] || {
    echo "release SHA must be a full lowercase 40-character Git SHA" >&2
    exit 64
  }
}

begin_critical_phase () {
  # The systemd job and link transaction must reach a terminal state before an
  # operator interrupt is acted upon. Record the first signal and return its
  # conventional status only after the transaction has completed or recovered.
  DEFERRED_SIGNAL=0
  trap '[ "$DEFERRED_SIGNAL" -ne 0 ] || DEFERRED_SIGNAL=129' HUP
  trap '[ "$DEFERRED_SIGNAL" -ne 0 ] || DEFERRED_SIGNAL=130' INT
  trap '[ "$DEFERRED_SIGNAL" -ne 0 ] || DEFERRED_SIGNAL=143' TERM
}

end_critical_phase () {
  local deferred=$DEFERRED_SIGNAL
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  DEFERRED_SIGNAL=0
  [ "$deferred" -eq 0 ] || return "$deferred"
}

critical_activate_release () {
  begin_critical_phase
  local rc=0
  if ! activate_release "$@"; then
    rc=$?
    # `!` reports zero inside this branch; retain a deterministic activation
    # failure status while activate_release logs the specific cause.
    rc=1
  fi
  local deferred_rc=0
  end_critical_phase || deferred_rc=$?
  [ "$deferred_rc" -eq 0 ] || return "$deferred_rc"
  return "$rc"
}

restore_activation_selection () {
  [ "$ACTIVATION_PENDING" -eq 1 ] || return 0
  local marker_state=${1:-blocked}
  local backup_manifest=${2:-none}
  local content_manifest=${3:-none}
  local selected=
  if [ -L "$CURRENT_LINK" ]; then
    if ! selected=$(readlink -f -- "$CURRENT_LINK"); then
      echo "CRITICAL: could not resolve current selection during activation recovery" >&2
      return 1
    fi
  elif [ -e "$CURRENT_LINK" ]; then
    echo "CRITICAL: current selection became a non-symlink during activation" >&2
    return 1
  fi
  if [ "$selected" = "$ACTIVATION_TARGET" ]; then
    if [ "$ACTIVATION_HAD_CURRENT" -eq 1 ]; then
      local current_restore="${CURRENT_LINK}.restore.$$"
      [ ! -e "$current_restore" ] && [ ! -L "$current_restore" ] || return 1
      if ! ln -s -- "$ACTIVATION_OLD" "$current_restore"; then
        return 1
      fi
      ACTIVATION_TEMP_LINKS+=("$current_restore")
      if ! mv -T -- "$current_restore" "$CURRENT_LINK"; then
        rm -f -- "$current_restore" || true
        return 1
      fi
    else
      rm -f -- "$CURRENT_LINK" || return 1
    fi
  elif [ "$ACTIVATION_HAD_CURRENT" -eq 1 ] && [ "$selected" = "$ACTIVATION_OLD" ]; then
    :
  elif [ "$ACTIVATION_HAD_CURRENT" -eq 0 ] && [ -z "$selected" ]; then
    :
  else
    echo "CRITICAL: current selection changed outside the activation transaction" >&2
    return 1
  fi
  if [ "$ACTIVATION_HAD_PREVIOUS" -eq 1 ]; then
    local previous_restore="${PREVIOUS_LINK}.restore.$$"
    [ ! -e "$previous_restore" ] && [ ! -L "$previous_restore" ] || return 1
    if ! ln -s -- "$ACTIVATION_PRIOR_PREVIOUS" "$previous_restore"; then
      return 1
    fi
    ACTIVATION_TEMP_LINKS+=("$previous_restore")
    if ! mv -T -- "$previous_restore" "$PREVIOUS_LINK"; then
      rm -f -- "$previous_restore" || true
      return 1
    fi
  else
    rm -f -- "$PREVIOUS_LINK" || return 1
  fi
  if ! sync -f /opt/deltamud; then
    echo "CRITICAL: restored release links could not be made durable" >&2
    return 1
  fi
  local restored_target=none
  [ "$ACTIVATION_HAD_CURRENT" -eq 1 ] && restored_target=$ACTIVATION_OLD
  local journal_target=$restored_target
  [ "$marker_state" = blocked ] && journal_target=$ACTIVATION_TARGET
  write_activation_marker "$marker_state" "$journal_target" "$backup_manifest" \
    "$content_manifest" || return 1
  ACTIVATION_PENDING=0
  ACTIVATION_RECOVERY_MANIFEST=none
  ACTIVATION_CONTENT_MANIFEST=none
}

cleanup_release () {
  rc=$?
  trap - EXIT
  trap '' HUP INT TERM
  set +e
  if [ "$ACTIVATION_PENDING" -eq 1 ]; then
    if ! stop_service_and_wait; then
      echo "CRITICAL: activation cleanup could not prove the candidate stopped; links were not changed" >&2
      [ "$rc" -ne 0 ] || rc=70
    elif ! restore_activation_selection blocked "$ACTIVATION_RECOVERY_MANIFEST" \
      "$ACTIVATION_CONTENT_MANIFEST"; then
      echo "CRITICAL: activation cleanup could not durably restore release selection" >&2
      [ "$rc" -ne 0 ] || rc=70
    fi
  fi
  if ! drain_build_user_processes && [ "$rc" -eq 0 ]; then
    rc=70
  fi
  for link in "${ACTIVATION_TEMP_LINKS[@]}"; do
    case "$link" in
      "$CURRENT_LINK".next.*|"$CURRENT_LINK".rollback.*|"$CURRENT_LINK".restore.*|"$PREVIOUS_LINK".next.*|"$PREVIOUS_LINK".restore.*)
        rm -f -- "$link"
        ;;
    esac
  done
  if [ -n "$DEPLOY_STAGE" ]; then
    case "$DEPLOY_STAGE" in
      "$RELEASE_ROOT"/.stage.*) rm -rf -- "$DEPLOY_STAGE" ;;
    esac
  fi
  if [ -n "$BUILD_WORK" ]; then
    case "$BUILD_WORK" in
      /var/tmp/deltamud-release-build.*) rm -rf -- "$BUILD_WORK" ;;
    esac
  fi
  if [ -n "$CANARY_ARTIFACT_PARENT" ]; then
    case "$CANARY_ARTIFACT_PARENT" in
      /var/tmp/deltamud-release-canary.*) rm -rf -- "$CANARY_ARTIFACT_PARENT" ;;
    esac
  fi
  if [ -n "$APPROVAL_TEMP" ]; then
    case "$APPROVAL_TEMP" in
      /etc/deltamud/.content-approved.*) rm -f -- "$APPROVAL_TEMP" ;;
    esac
  fi
  if [ -n "$ACTIVATION_MARKER_TEMP" ]; then
    case "$ACTIVATION_MARKER_TEMP" in
      "$ACTIVATION_ROOT"/.pending.*) rm -f -- "$ACTIVATION_MARKER_TEMP" ;;
    esac
  fi
  if [ -n "$RESTORE_CHECK_SCHEMA" ] && [ -f "$BACKUP_CNF" ]; then
    if ! mariadb --defaults-file="$BACKUP_CNF" \
      -e "DROP DATABASE IF EXISTS \`$RESTORE_CHECK_SCHEMA\`" >/dev/null 2>&1; then
      echo "CRITICAL: restore-check database cleanup failed: $RESTORE_CHECK_SCHEMA" >&2
      echo "remove that exact database with $BACKUP_CNF after investigating" >&2
      [ "$rc" -ne 0 ] || rc=70
    else
      RESTORE_CHECK_SCHEMA=
    fi
  fi
  if [ -n "$BACKUP_STAGE" ]; then
    case "$BACKUP_STAGE" in
      "$BACKUP_ROOT"/backup.*) rm -rf -- "$BACKUP_STAGE" ;;
    esac
  fi
  exit "$rc"
}

acquire_release_lock () {
  command -v flock >/dev/null 2>&1 || {
    echo "release serialization requires flock" >&2
    exit 77
  }
  validate_root_directory /run || exit 77
  install -d -o root -g root -m 0700 /run/deltamud-release
  validate_root_directory /run/deltamud-release || exit 77
  local lock_file=/run/deltamud-release/release.lock
  if [ -e "$lock_file" ] || [ -L "$lock_file" ]; then
    [ -f "$lock_file" ] && [ ! -L "$lock_file" ] \
      && [ "$(stat -Lc %u -- "$lock_file")" -eq 0 ] \
      && [ "$((8#$(stat -Lc %a -- "$lock_file") & 8#022))" -eq 0 ] || {
        echo "refusing unsafe release lock file: $lock_file" >&2
        exit 77
      }
  fi
  exec 9>"$lock_file"
  chmod 0600 "$lock_file"
  flock --nonblock 9 || {
    echo "another DeltaMUD release operation owns $lock_file" >&2
    exit 75
  }
}

validate_root_directory () {
  local path=$1
  [ -d "$path" ] && [ ! -L "$path" ] || {
    echo "required path is not a real directory: $path" >&2
    return 1
  }
  [ "$(stat -Lc %u -- "$path")" -eq 0 ] || {
    echo "required path is not root-owned: $path" >&2
    return 1
  }
  local mode
  mode=$(stat -Lc %a -- "$path")
  [ "$((8#$mode & 8#022))" -eq 0 ] || {
    echo "required path is group/world writable: $path" >&2
    return 1
  }
}

validate_root_file () {
  local path=$1
  [ -f "$path" ] && [ ! -L "$path" ] \
    && [ "$(stat -Lc %u -- "$path")" -eq 0 ] || {
      echo "required file is missing, linked, or not root-owned: $path" >&2
      return 1
    }
  local mode
  mode=$(stat -Lc %a -- "$path")
  [ "$((8#$mode & 8#022))" -eq 0 ] || {
    echo "required file is group/world writable: $path" >&2
    return 1
  }
}

ensure_activation_root () {
  validate_root_directory /etc || return 1
  validate_root_directory /etc/deltamud || return 1
  if [ ! -e "$ACTIVATION_ROOT" ] && [ ! -L "$ACTIVATION_ROOT" ]; then
    install -d -o root -g root -m 0700 "$ACTIVATION_ROOT" || return 1
  fi
  validate_root_directory "$ACTIVATION_ROOT" || return 1
  [ "$(stat -Lc %u:%g:%a -- "$ACTIVATION_ROOT")" = 0:0:700 ] || {
    echo "activation control directory must be root:root mode 0700: $ACTIVATION_ROOT" >&2
    return 1
  }
}

activation_marker_value () {
  local key=$1
  awk -F= -v wanted="$key" '
    $1 == wanted { value = substr($0, length($1) + 2); count++ }
    END { if (count == 1) print value; else exit 1 }
  ' "$ACTIVATION_MARKER"
}

validate_activation_marker_file () {
  ensure_activation_root || return 1
  [ "$(readlink -f -- "$ACTIVATION_MARKER" 2>/dev/null || true)" = "$ACTIVATION_MARKER" ] \
    && validate_root_file "$ACTIVATION_MARKER" || {
      echo "activation marker is missing, linked, or unsafe: $ACTIVATION_MARKER" >&2
      return 1
    }
  [ "$(stat -Lc %a -- "$ACTIVATION_MARKER")" = 600 ] || {
    echo "activation marker must be mode 0600" >&2
    return 1
  }
  LC_ALL=C awk -F= '
    $1 == "format" || $1 == "state" || $1 == "attempt" || $1 == "target" \
      || $1 == "binary_sha256" || $1 == "boot_id" || $1 == "manager_pid" \
      || $1 == "manager_starttime" || $1 == "backup_manifest" \
      || $1 == "content_rollback_manifest" { seen[$1]++; next }
    { bad = 1 }
    END {
      required[1] = "format"; required[2] = "state"; required[3] = "attempt"
      required[4] = "target"; required[5] = "binary_sha256"; required[6] = "boot_id"
      required[7] = "manager_pid"; required[8] = "manager_starttime"
      required[9] = "backup_manifest"; required[10] = "content_rollback_manifest"
      for (i = 1; i <= 10; i++) if (seen[required[i]] != 1) bad = 1
      exit bad ? 1 : 0
    }
  ' "$ACTIVATION_MARKER" || {
    echo "activation marker has an invalid field set" >&2
    return 1
  }
  [ "$(activation_marker_value format)" = deltamud-activation-v2 ] || {
    echo "activation marker has an unsupported format" >&2
    return 1
  }
}

validate_marker_target () {
  local target=$1
  local expected_hash=$2
  [ "$target" != none ] || return 0
  validate_release_reference "$target" || return 1
  local selected=
  if [ -L "$CURRENT_LINK" ]; then
    selected=$(readlink -f -- "$CURRENT_LINK") || return 1
  fi
  [ "$selected" = "$target" ] || {
    echo "activation marker target does not match $CURRENT_LINK" >&2
    return 1
  }
  [[ "$expected_hash" =~ ^[0-9a-f]{64}$ ]] \
    && [ "$expected_hash" = "$(sha256sum -- "$target/bin/deltamud" | awk '{print $1}')" ] || {
      echo "activation marker binary hash is invalid" >&2
      return 1
    }
}

publish_activation_marker_fields () {
  local state=$1
  local attempt=$2
  local target=$3
  local binary_hash=$4
  local boot_id=$5
  local manager_pid=$6
  local manager_starttime=$7
  local backup_manifest=$8
  local content_manifest=$9
  if ! ACTIVATION_MARKER_TEMP=$(mktemp "$ACTIVATION_ROOT/.pending.XXXXXX"); then
    echo "could not create the activation marker stage" >&2
    return 1
  fi
  if ! printf '%s\n' \
    'format=deltamud-activation-v2' \
    "state=$state" \
    "attempt=$attempt" \
    "target=$target" \
    "binary_sha256=$binary_hash" \
    "boot_id=$boot_id" \
    "manager_pid=$manager_pid" \
    "manager_starttime=$manager_starttime" \
    "backup_manifest=$backup_manifest" \
    "content_rollback_manifest=$content_manifest" >"$ACTIVATION_MARKER_TEMP" \
    || ! chmod 0600 "$ACTIVATION_MARKER_TEMP" \
    || ! sync -f "$ACTIVATION_MARKER_TEMP" \
    || ! mv -T -- "$ACTIVATION_MARKER_TEMP" "$ACTIVATION_MARKER" \
    || ! sync -f "$ACTIVATION_ROOT"; then
    case "$ACTIVATION_MARKER_TEMP" in
      "$ACTIVATION_ROOT"/.pending.*) rm -f -- "$ACTIVATION_MARKER_TEMP" || true ;;
    esac
    ACTIVATION_MARKER_TEMP=
    echo "could not durably publish the activation marker" >&2
    return 1
  fi
  ACTIVATION_MARKER_TEMP=
}

write_activation_marker () {
  local state=$1
  local target=$2
  local backup_manifest=${3:-none}
  local content_manifest=${4:-none}
  local attempt=blocked
  local binary_hash=none
  local boot_id=none
  local manager_pid=0
  local manager_starttime=0
  ensure_activation_root || return 1
  case "$state" in
    pending|migrating)
      [ "$target" != none ] && validate_release_reference "$target" || return 1
      attempt=ready
      binary_hash=$(sha256sum -- "$target/bin/deltamud" | awk '{print $1}') || return 1
      boot_id=$(tr -d '\n' </proc/sys/kernel/random/boot_id) || return 1
      manager_pid=$$
      manager_starttime=$(awk '{print $22}' "/proc/$$/stat") || return 1
      [[ "$boot_id" =~ ^[0-9a-f-]{36}$ ]] \
        && [[ "$manager_starttime" =~ ^[1-9][0-9]*$ ]] || return 1
      ;;
    blocked|maintenance)
      if [ "$target" != none ]; then
        validate_release_reference "$target" || return 1
        binary_hash=$(sha256sum -- "$target/bin/deltamud" | awk '{print $1}') || return 1
      fi
      ;;
    *)
      echo "refusing invalid activation-marker state: $state" >&2
      return 1
      ;;
  esac
  if [ "$backup_manifest" != none ]; then
    case "$backup_manifest" in
      "$BACKUP_ROOT"/backup.*/manifest) ;;
      *) echo "activation marker backup is outside the fixed backup root" >&2; return 1 ;;
    esac
  fi
  if [ "$content_manifest" != none ]; then
    case "$content_manifest" in
      "$BACKUP_ROOT"/backup.*/manifest) ;;
      *) echo "activation marker content rollback is outside the fixed backup root" >&2; return 1 ;;
    esac
  fi
  publish_activation_marker_fields "$state" "$attempt" "$target" "$binary_hash" \
    "$boot_id" "$manager_pid" "$manager_starttime" "$backup_manifest" \
    "$content_manifest"
}

clear_activation_marker () {
  local expected_target=$1
  validate_activation_marker_file || return 1
  [ "$(activation_marker_value state)" = confirmed ] \
    && [ "$(activation_marker_value target)" = "$expected_target" ] || {
      echo "activation marker is not a confirmation for $expected_target" >&2
      return 1
    }
  rm -f -- "$ACTIVATION_MARKER" || return 1
  sync -f "$ACTIVATION_ROOT" || return 1
}

ensure_no_activation_marker () {
  if [ -e "$ACTIVATION_MARKER" ] || [ -L "$ACTIVATION_MARKER" ]; then
    echo "an earlier activation marker requires recovery: $ACTIVATION_MARKER" >&2
    echo "use activation-recover for a confirmed live target, or follow the runbook's activation-resolve workflow" >&2
    return 1
  fi
}

enter_maintenance_window () {
  local target=$1
  # A pre-existing marker is recovery evidence, even when it names this target
  # or has not yet acquired a backup. Only an explicitly acknowledged resolver
  # (or the narrowly validated migration-resume path) may consume it.
  ensure_no_activation_marker || return 1
  write_activation_marker maintenance "$target" none none
}

update_maintenance_window () {
  local target=$1
  local backup_manifest=${2:-none}
  local content_manifest=${3:-none}
  validate_activation_marker_file || return 1
  [ "$(activation_marker_value state)" = maintenance ] \
    && [ "$(activation_marker_value target)" = "$target" ] || return 1
  write_activation_marker maintenance "$target" "$backup_manifest" "$content_manifest"
}

clear_maintenance_window () {
  local target=$1
  validate_activation_marker_file || return 1
  [ "$(activation_marker_value state)" = maintenance ] \
    && [ "$(activation_marker_value target)" = "$target" ] || return 1
  rm -f -- "$ACTIVATION_MARKER" || return 1
  sync -f "$ACTIVATION_ROOT"
}

activation_guard () {
  [ "$(id -u)" -eq 0 ] || return 1
  if [ ! -e "$ACTIVATION_MARKER" ] && [ ! -L "$ACTIVATION_MARKER" ]; then
    local selected
    selected=$(readlink -f -- "$CURRENT_LINK") || return 1
    validate_release_reference "$selected"
    return
  fi
  validate_activation_marker_file || return 1
  local state attempt target binary_hash boot_id manager_pid manager_starttime backup_manifest content_manifest
  state=$(activation_marker_value state) || return 1
  attempt=$(activation_marker_value attempt) || return 1
  target=$(activation_marker_value target) || return 1
  binary_hash=$(activation_marker_value binary_sha256) || return 1
  boot_id=$(activation_marker_value boot_id) || return 1
  manager_pid=$(activation_marker_value manager_pid) || return 1
  manager_starttime=$(activation_marker_value manager_starttime) || return 1
  backup_manifest=$(activation_marker_value backup_manifest) || return 1
  content_manifest=$(activation_marker_value content_rollback_manifest) || return 1
  if [ "$state" = blocked ] || [ "$state" = migrating ] || [ "$state" = maintenance ]; then
    return 1
  fi
  validate_marker_target "$target" "$binary_hash" || return 1
  if [ "$state" = confirmed ]; then
    return 0
  fi
  [ "$state" = pending ] && [ "$attempt" = ready ] \
    && [[ "$manager_pid" =~ ^[1-9][0-9]*$ ]] \
    && [[ "$manager_starttime" =~ ^[1-9][0-9]*$ ]] || return 1
  [ "$boot_id" = "$(tr -d '\n' </proc/sys/kernel/random/boot_id)" ] || return 1
  [ -r "/proc/$manager_pid/stat" ] \
    && [ "$(awk '{print $22}' "/proc/$manager_pid/stat")" = "$manager_starttime" ] || return 1
  tr '\0' '\n' <"/proc/$manager_pid/cmdline" | grep -Fxq "$TRUSTED_MANAGER" || return 1
  publish_activation_marker_fields pending consumed "$target" "$binary_hash" \
    "$boot_id" "$manager_pid" "$manager_starttime" "$backup_manifest" "$content_manifest"
}

pid_owns_ready_listener () {
  local pid=$1
  [[ "$pid" =~ ^[1-9][0-9]*$ ]] && [ -d "/proc/$pid/fd" ] || return 1
  local socket_inodes inode descriptor link
  socket_inodes=$(awk '
    $2 == "0100007F:4C8B" && $4 == "0A" && $10 ~ /^[0-9]+$/ { print $10 }
  ' "/proc/$pid/net/tcp" 2>/dev/null) || return 1
  [ -n "$socket_inodes" ] || return 1
  while IFS= read -r inode; do
    for descriptor in "/proc/$pid/fd/"*; do
      link=$(readlink -- "$descriptor" 2>/dev/null || true)
      [ "$link" = "socket:[$inode]" ] && return 0
    done
  done <<<"$socket_inodes"
  return 1
}

ready_response_from_pid () {
  local pid=$1
  local expected_binary=$2
  local observed_pid observed_binary
  pid_owns_ready_listener "$pid" || return 1
  curl --disable --noproxy '*' --proto '=http' --fail --silent --show-error \
    --connect-timeout 0.2 --max-time 0.5 "$READY_URL" >/dev/null || return 1
  observed_pid=$(systemctl show "$SERVICE" --property=MainPID --value 2>/dev/null || true)
  [ "$observed_pid" = "$pid" ] || return 1
  observed_binary=$(readlink -f -- "/proc/$pid/exe" 2>/dev/null || true)
  [ "$observed_binary" = "$expected_binary" ] || return 1
  pid_owns_ready_listener "$pid"
}

activation_confirm () {
  [ "$(id -u)" -eq 0 ] || return 1
  local rewrite_confirmation=0
  local state attempt target binary_hash boot_id manager_pid manager_starttime backup_manifest content_manifest
  if [ ! -e "$ACTIVATION_MARKER" ] && [ ! -L "$ACTIVATION_MARKER" ]; then
    target=$(readlink -f -- "$CURRENT_LINK") || return 1
    validate_release_reference "$target" || return 1
    state=unjournaled
    attempt=none
    binary_hash=$(sha256sum -- "$target/bin/deltamud" | awk '{print $1}') || return 1
    boot_id=none
    manager_pid=0
    manager_starttime=0
    backup_manifest=none
    content_manifest=none
  else
    validate_activation_marker_file || return 1
    state=$(activation_marker_value state) || return 1
    attempt=$(activation_marker_value attempt) || return 1
    target=$(activation_marker_value target) || return 1
    binary_hash=$(activation_marker_value binary_sha256) || return 1
    boot_id=$(activation_marker_value boot_id) || return 1
    manager_pid=$(activation_marker_value manager_pid) || return 1
    manager_starttime=$(activation_marker_value manager_starttime) || return 1
    backup_manifest=$(activation_marker_value backup_manifest) || return 1
    content_manifest=$(activation_marker_value content_rollback_manifest) || return 1
    validate_marker_target "$target" "$binary_hash" || return 1
    case "$state:$attempt" in
      confirmed:consumed) ;;
      pending:consumed) rewrite_confirmation=1 ;;
      *) return 1 ;;
    esac
  fi
  local attempt_number main_pid running_binary
  for ((attempt_number = 0; attempt_number < 50; attempt_number++)); do
    main_pid=$(systemctl show "$SERVICE" --property=MainPID --value 2>/dev/null || true)
    running_binary=
    if [[ "$main_pid" =~ ^[1-9][0-9]*$ ]]; then
      running_binary=$(readlink -f -- "/proc/$main_pid/exe" 2>/dev/null || true)
    fi
    if [ "$running_binary" = "$target/bin/deltamud" ] \
      && ready_response_from_pid "$main_pid" "$target/bin/deltamud"; then
      if [ "$rewrite_confirmation" -eq 1 ]; then
        publish_activation_marker_fields confirmed consumed "$target" "$binary_hash" \
          "$boot_id" "$manager_pid" "$manager_starttime" "$backup_manifest" "$content_manifest"
        return $?
      fi
      return 0
    fi
    sleep 0.2 || true
  done
  echo "candidate did not become ready as the exact journaled binary" >&2
  return 1
}

migration_guard () {
  local revision=$1
  validate_sha "$revision"
  [ "$(id -u)" -eq 0 ] || return 1
  validate_runtime_environment || return 1
  validate_migration_unit "$revision" || return 1
  validate_activation_marker_file || return 1

  local state attempt target binary_hash boot_id manager_pid manager_starttime
  local backup_manifest content_manifest lock_file=/run/deltamud-release/release.lock
  state=$(activation_marker_value state) || return 1
  attempt=$(activation_marker_value attempt) || return 1
  target=$(activation_marker_value target) || return 1
  binary_hash=$(activation_marker_value binary_sha256) || return 1
  boot_id=$(activation_marker_value boot_id) || return 1
  manager_pid=$(activation_marker_value manager_pid) || return 1
  manager_starttime=$(activation_marker_value manager_starttime) || return 1
  backup_manifest=$(activation_marker_value backup_manifest) || return 1
  content_manifest=$(activation_marker_value content_rollback_manifest) || return 1
  [ "$state" = migrating ] && [ "$attempt" = ready ] \
    && [ "$target" = "$RELEASE_ROOT/$revision" ] \
    && [[ "$binary_hash" =~ ^[0-9a-f]{64}$ ]] \
    && [ "$binary_hash" = "$(sha256sum -- "$target/bin/deltamud" | awk '{print $1}')" ] \
    && [ "$boot_id" = "$(tr -d '\n' </proc/sys/kernel/random/boot_id)" ] \
    && [[ "$manager_pid" =~ ^[1-9][0-9]*$ ]] \
    && [[ "$manager_starttime" =~ ^[1-9][0-9]*$ ]] || return 1
  validate_release_reference "$target" || return 1
  [ "$backup_manifest" != none ] \
    && validate_backup_manifest "$backup_manifest" "$revision" any || return 1
  if [ "$content_manifest" != none ]; then
    validate_backup_manifest "$content_manifest" "$revision" any || return 1
  fi
  [ -r "/proc/$manager_pid/stat" ] \
    && [ "$(awk '{print $22}' "/proc/$manager_pid/stat")" = "$manager_starttime" ] \
    && awk '/^Uid:/ { found = 1; ok = ($2 == 0 && $3 == 0 && $4 == 0 && $5 == 0) } \
      END { exit !(found && ok) }' \
      "/proc/$manager_pid/status" \
    && tr '\0' '\n' <"/proc/$manager_pid/cmdline" | grep -Fxq "$TRUSTED_MANAGER" \
    || return 1
  validate_root_directory /run || return 1
  validate_root_directory /run/deltamud-release || return 1
  validate_root_file "$lock_file" || return 1
  [ "$(readlink -f -- "/proc/$manager_pid/fd/9" 2>/dev/null || true)" = "$lock_file" ] \
    && grep -Eq '^lock:.*FLOCK[[:space:]]+ADVISORY[[:space:]]+WRITE' \
      "/proc/$manager_pid/fdinfo/9" || return 1
  verify_service_stopped || return 1
  # Close the direct-systemctl replay window: only one start attempt may
  # consume this live manager's durable authorization.
  [ "$(awk '{print $22}' "/proc/$manager_pid/stat" 2>/dev/null || true)" \
    = "$manager_starttime" ] || return 1
  publish_activation_marker_fields migrating consumed "$target" "$binary_hash" \
    "$boot_id" "$manager_pid" "$manager_starttime" "$backup_manifest" "$content_manifest"
}

validate_unit_file () {
  local unit=$1
  local expected_hash=$2
  local fragment dropins need_reload
  if ! fragment=$(systemctl show "$unit" --property=FragmentPath --value); then
    echo "could not resolve the installed systemd unit: $unit" >&2
    return 1
  fi
  if ! dropins=$(systemctl show "$unit" --property=DropInPaths --value); then
    echo "could not resolve systemd drop-ins for $unit" >&2
    return 1
  fi
  if ! need_reload=$(systemctl show "$unit" --property=NeedDaemonReload --value); then
    echo "could not determine whether $unit needs daemon-reload" >&2
    return 1
  fi
  [ "$need_reload" = no ] || {
    echo "$unit has changed on disk; run systemctl daemon-reload before release work" >&2
    return 1
  }
  [ -n "$fragment" ] && validate_root_file "$fragment" || {
    echo "installed systemd unit is missing or unsafe: $unit" >&2
    return 1
  }
  [ -z "$dropins" ] || {
    echo "systemd drop-ins are forbidden for release-controlled unit $unit: $dropins" >&2
    return 1
  }
  [ "$(sha256sum -- "$fragment" | awk '{print $1}')" = "$expected_hash" ] || {
    echo "installed systemd unit does not match this release manager: $unit" >&2
    return 1
  }
}

validate_main_unit () {
  validate_unit_file "$SERVICE" "$MAIN_UNIT_SHA256" || return 1
  local user group environment exec_condition exec_start exec_start_post
  local send_sigkill no_new_privileges protect_system read_write_paths unset_environment
  user=$(systemctl show "$SERVICE" --property=User --value)
  group=$(systemctl show "$SERVICE" --property=Group --value)
  environment=$(systemctl show "$SERVICE" --property=EnvironmentFiles --value)
  exec_condition=$(systemctl show "$SERVICE" --property=ExecCondition --value)
  exec_start=$(systemctl show "$SERVICE" --property=ExecStart --value)
  exec_start_post=$(systemctl show "$SERVICE" --property=ExecStartPost --value)
  send_sigkill=$(systemctl show "$SERVICE" --property=SendSIGKILL --value)
  no_new_privileges=$(systemctl show "$SERVICE" --property=NoNewPrivileges --value)
  protect_system=$(systemctl show "$SERVICE" --property=ProtectSystem --value)
  read_write_paths=$(systemctl show "$SERVICE" --property=ReadWritePaths --value)
  unset_environment=$(systemctl show "$SERVICE" --property=UnsetEnvironment --value)
  [ "$user" = deltamud ] && [ "$group" = deltamud ] \
    && [ "$send_sigkill" = no ] && [ "$no_new_privileges" = yes ] \
    && [ "$protect_system" = strict ] \
    && [[ "$environment" == *"/etc/deltamud/deltamud.env"* ]] \
    && [[ "$exec_condition" == *"path=/usr/local/sbin/deltamud-release"* ]] \
    && [[ "$exec_condition" == *"argv[]=/usr/local/sbin/deltamud-release activation-guard"* ]] \
    && [[ "$exec_start" == *"path=/opt/deltamud/current/bin/deltamud"* ]] \
    && [[ "$exec_start" == *"argv[]=/opt/deltamud/current/bin/deltamud"* ]] \
    && [[ "$exec_start_post" == *"path=/usr/local/sbin/deltamud-release"* ]] \
    && [[ "$exec_start_post" == *"argv[]=/usr/local/sbin/deltamud-release activation-confirm"* ]] \
    && [[ "$read_write_paths" == *"/etc/deltamud/activation"* ]] \
    && [[ "$unset_environment" == *"LD_PRELOAD"* ]] \
    && [[ "$unset_environment" == *"BASH_ENV"* ]] || {
      echo "effective $SERVICE identity, stop policy, environment, hardening, or ExecStart is unsafe" >&2
      return 1
    }
}

validate_migration_unit () {
  local revision=$1
  local unit="deltamud-migrate@${revision}.service"
  local target="$RELEASE_ROOT/$revision/bin/deltamud"
  validate_unit_file "$unit" "$MIGRATION_UNIT_SHA256" || return 1
  local user group environment exec_condition exec_start no_new_privileges protect_system
  local timeout_start read_write_paths unset_environment
  user=$(systemctl show "$unit" --property=User --value)
  group=$(systemctl show "$unit" --property=Group --value)
  environment=$(systemctl show "$unit" --property=EnvironmentFiles --value)
  exec_condition=$(systemctl show "$unit" --property=ExecCondition --value)
  exec_start=$(systemctl show "$unit" --property=ExecStart --value)
  no_new_privileges=$(systemctl show "$unit" --property=NoNewPrivileges --value)
  protect_system=$(systemctl show "$unit" --property=ProtectSystem --value)
  timeout_start=$(systemctl show "$unit" --property=TimeoutStartUSec --value)
  read_write_paths=$(systemctl show "$unit" --property=ReadWritePaths --value)
  unset_environment=$(systemctl show "$unit" --property=UnsetEnvironment --value)
  [ "$user" = deltamud ] && [ "$group" = deltamud ] \
    && [ "$no_new_privileges" = yes ] && [ "$protect_system" = strict ] \
    && [ "$timeout_start" = infinity ] \
    && [[ "$environment" == *"/etc/deltamud/deltamud.env"* ]] \
    && [[ "$exec_condition" == *"path=/usr/local/sbin/deltamud-release"* ]] \
    && [[ "$exec_condition" == *"argv[]=/usr/local/sbin/deltamud-release migration-guard $revision"* ]] \
    && [[ "$exec_start" == *"path=$target"* ]] \
    && [[ "$exec_start" == *"argv[]=$target --migrate"* ]] \
    && [[ "$read_write_paths" == *"/etc/deltamud/activation"* ]] \
    && [[ "$unset_environment" == *"LD_PRELOAD"* ]] \
    && [[ "$unset_environment" == *"BASH_ENV"* ]] || {
      echo "effective $unit identity, environment, hardening, or ExecStart is unsafe" >&2
      return 1
    }
}

verify_frozen_source_snapshot () {
  [ -n "$SOURCE_SNAPSHOT_SHA256" ] && [ -d "$SOURCE_ROOT" ] || return 1
  local unsafe_source
  unsafe_source=$(find "$SOURCE_ROOT" -xdev \
    \( ! -user root -o -perm /022 -o \! -type d -a \! -type f -o -type f -links +1 \) \
    -print -quit) || return 1
  [ -z "$unsafe_source" ] \
    && [ "$(content_digest "$SOURCE_ROOT")" = "$SOURCE_SNAPSHOT_SHA256" ] || {
      echo "frozen exact-SHA source snapshot changed during release admission" >&2
      return 1
    }
}

validate_installed_release () {
  local release=$1
  local expected_sha=$2
  case "$release" in
    "$RELEASE_ROOT/$expected_sha") ;;
    *) echo "release path does not match its revision: $release" >&2; return 1 ;;
  esac
  validate_root_directory "$release" || return 1
  local unsafe_release
  if ! unsafe_release=$(find "$release" -xdev \
    \( ! -user root -o -perm /022 -o -type l \) -print); then
    echo "could not inspect installed release ownership and links: $release" >&2
    return 1
  fi
  if [ -n "$unsafe_release" ]; then
    echo "installed release is not entirely root-owned, non-writable, and link-free: $release" >&2
    return 1
  fi
  [ -f "$release/REVISION" ] && [ ! -L "$release/REVISION" ] \
    && [ "$(cat -- "$release/REVISION")" = "$expected_sha" ] || {
      echo "release revision marker is missing or mismatched: $release" >&2
      return 1
    }
  [ -f "$release/bin/deltamud" ] && [ ! -L "$release/bin/deltamud" ] \
    && [ -x "$release/bin/deltamud" ] \
    && [ "$(stat -Lc %u -- "$release/bin/deltamud")" -eq 0 ] || {
      echo "release binary is missing, linked, non-executable, or not root-owned: $release" >&2
      return 1
    }
  local binary_mode
  binary_mode=$(stat -Lc %a -- "$release/bin/deltamud")
  [ "$((8#$binary_mode & 8#022))" -eq 0 ] || {
    echo "release binary is group/world writable: $release/bin/deltamud" >&2
    return 1
  }
  [ -f "$release/CONTENT_SHA256" ] && [ ! -L "$release/CONTENT_SHA256" ] \
    && [[ "$(cat -- "$release/CONTENT_SHA256")" =~ ^[0-9a-f]{64}$ ]] || {
      echo "release content digest is missing or invalid: $release" >&2
      return 1
    }
  [ -f "$release/RELEASE_MANIFEST" ] && [ ! -L "$release/RELEASE_MANIFEST" ] || {
    echo "release integrity manifest is missing: $release" >&2
    return 1
  }
  LC_ALL=C awk -F= '
    $1 == "format" || $1 == "revision" || $1 == "binary_sha256" \
      || $1 == "content_sha256" { seen[$1]++; next }
    { bad = 1 }
    END {
      if (seen["format"] != 1 || seen["revision"] != 1 \
          || seen["binary_sha256"] != 1 || seen["content_sha256"] != 1) bad = 1
      exit bad ? 1 : 0
    }
  ' "$release/RELEASE_MANIFEST" || return 1
  local manifest_format manifest_revision manifest_binary manifest_content actual_binary actual_content
  manifest_format=$(awk -F= '$1 == "format" { print substr($0, 8) }' \
    "$release/RELEASE_MANIFEST") || return 1
  manifest_revision=$(awk -F= '$1 == "revision" { print substr($0, 10); count++ } END { if (count != 1) exit 1 }' \
    "$release/RELEASE_MANIFEST") || return 1
  manifest_binary=$(awk -F= '$1 == "binary_sha256" { print substr($0, 15); count++ } END { if (count != 1) exit 1 }' \
    "$release/RELEASE_MANIFEST") || return 1
  manifest_content=$(awk -F= '$1 == "content_sha256" { print substr($0, 16); count++ } END { if (count != 1) exit 1 }' \
    "$release/RELEASE_MANIFEST") || return 1
  actual_binary=$(sha256sum -- "$release/bin/deltamud" | awk '{print $1}') || return 1
  actual_content=$(content_digest "$release/content/lib") || return 1
  [ "$manifest_format" = deltamud-release-v2 ] \
    && [ "$manifest_revision" = "$expected_sha" ] \
    && [[ "$manifest_binary" =~ ^[0-9a-f]{64}$ ]] \
    && [[ "$manifest_content" =~ ^[0-9a-f]{64}$ ]] \
    && [ "$manifest_binary" = "$actual_binary" ] \
    && [ "$manifest_content" = "$actual_content" ] \
    && [ "$manifest_content" = "$(cat -- "$release/CONTENT_SHA256")" ] || {
      echo "installed release integrity verification failed: $release" >&2
      return 1
    }
  [ -d "$release/canary-evidence" ] && [ ! -L "$release/canary-evidence" ] \
    && [ -f "$release/CANARY_MANIFEST" ] && [ ! -L "$release/CANARY_MANIFEST" ] || {
      echo "installed release canary evidence is missing: $release" >&2
      return 1
    }
  LC_ALL=C awk -F= '
    $1 == "format" || $1 == "revision" || $1 == "binary_sha256" \
      || $1 == "content_sha256" || $1 == "evidence_sha256" { seen[$1]++; next }
    { bad = 1 }
    END {
      if (seen["format"] != 1 || seen["revision"] != 1 \
          || seen["binary_sha256"] != 1 || seen["content_sha256"] != 1 \
          || seen["evidence_sha256"] != 1) bad = 1
      exit bad ? 1 : 0
    }
  ' "$release/CANARY_MANIFEST" || return 1
  local evidence_format evidence_revision evidence_binary evidence_content evidence_digest actual_evidence
  evidence_format=$(awk -F= '$1 == "format" { print substr($0, 8) }' \
    "$release/CANARY_MANIFEST") || return 1
  evidence_revision=$(awk -F= '$1 == "revision" { print substr($0, 10); count++ } END { if (count != 1) exit 1 }' \
    "$release/CANARY_MANIFEST") || return 1
  evidence_binary=$(awk -F= '$1 == "binary_sha256" { print substr($0, 15); count++ } END { if (count != 1) exit 1 }' \
    "$release/CANARY_MANIFEST") || return 1
  evidence_content=$(awk -F= '$1 == "content_sha256" { print substr($0, 16); count++ } END { if (count != 1) exit 1 }' \
    "$release/CANARY_MANIFEST") || return 1
  evidence_digest=$(awk -F= '$1 == "evidence_sha256" { print substr($0, 17); count++ } END { if (count != 1) exit 1 }' \
    "$release/CANARY_MANIFEST") || return 1
  actual_evidence=$(content_digest "$release/canary-evidence") || return 1
  [ "$evidence_format" = deltamud-canary-evidence-v1 ] \
    && [ "$evidence_revision" = "$expected_sha" ] \
    && [ "$evidence_binary" = "$actual_binary" ] \
    && [ "$evidence_content" = "$actual_content" ] \
    && [[ "$evidence_digest" =~ ^[0-9a-f]{64}$ ]] \
    && [ "$evidence_digest" = "$actual_evidence" ] || {
      echo "installed release canary evidence verification failed: $release" >&2
      return 1
    }
}

validate_release_reference () {
  local release=$1
  local revision=${release##*/}
  [[ "$revision" =~ ^[0-9a-f]{40}$ ]] || {
    echo "release reference has an invalid revision path: $release" >&2
    return 1
  }
  validate_installed_release "$release" "$revision"
}

build_user_pids () {
  [ -n "$BUILD_UID" ] || return 0
  local effective= real= rc
  if effective=$(pgrep -u "$BUILD_UID" 2>/dev/null); then
    :
  else
    rc=$?
    [ "$rc" -eq 1 ] || return "$rc"
    effective=
  fi
  if real=$(pgrep -U "$BUILD_UID" 2>/dev/null); then
    :
  else
    rc=$?
    [ "$rc" -eq 1 ] || return "$rc"
    real=
  fi
  { [ -z "$effective" ] || printf '%s\n' "$effective"; [ -z "$real" ] || printf '%s\n' "$real"; } \
    | sort -un
}

drain_build_user_processes () {
  [ -n "$BUILD_UID" ] || return 0
  local pids attempt
  pids=$(build_user_pids) || return 1
  [ -n "$pids" ] || return 0
  for ((attempt = 0; attempt < 30; attempt++)); do
    pids=$(build_user_pids) || return 1
    [ -n "$pids" ] || return 0
    # Every token came from /proc through pgrep and is numeric. Drop to the
    # dedicated UID before signaling, so PID reuse can never make this
    # privileged cleanup hit a process belonging to another account.
    setpriv --reuid="$BUILD_UID" --regid="$(id -g "$BUILD_USER")" --clear-groups \
      --no-new-privs --bounding-set=-all --inh-caps=-all --ambient-caps=-all \
      /bin/kill -TERM $pids 2>/dev/null || true
    sleep 0.1
  done
  for ((attempt = 0; attempt < 50; attempt++)); do
    pids=$(build_user_pids) || return 1
    [ -n "$pids" ] || return 0
    setpriv --reuid="$BUILD_UID" --regid="$(id -g "$BUILD_USER")" --clear-groups \
      --no-new-privs --bounding-set=-all --inh-caps=-all --ambient-caps=-all \
      /bin/kill -KILL $pids 2>/dev/null || true
    sleep 0.1
  done
  echo "dedicated build-user processes survived the cleanup deadline: $pids" >&2
  return 1
}

require_build_user_quiescent () {
  local pids
  pids=$(build_user_pids) || {
    echo "could not inspect the dedicated build UID's process set" >&2
    return 1
  }
  [ -z "$pids" ] && return 0
  echo "unexpected process(es) owned by the dedicated build UID: $pids" >&2
  drain_build_user_processes || true
  return 1
}

validate_build_toolchain () {
  validate_root_directory /opt/deltamud || exit 77
  validate_root_directory /opt/deltamud/toolchains || exit 77
  validate_root_directory "$TOOLCHAIN_ROOT" || exit 77
  local unsafe_toolchain
  if ! unsafe_toolchain=$(find "$TOOLCHAIN_ROOT" -xdev \
    \( ! -user root -o -perm /022 -o -type l \) -print); then
    echo "could not inspect the pinned build toolchain" >&2
    exit 77
  fi
  if [ -n "$unsafe_toolchain" ]; then
    echo "build toolchain must be entirely root-owned, non-writable, and link-free" >&2
    exit 77
  fi
  local tool
  for tool in cargo rustc rustfmt clippy-driver cargo-audit; do
    [ -f "$TOOLCHAIN_ROOT/bin/$tool" ] && [ ! -L "$TOOLCHAIN_ROOT/bin/$tool" ] \
      && [ -x "$TOOLCHAIN_ROOT/bin/$tool" ] || {
        echo "pinned build tool is missing: $TOOLCHAIN_ROOT/bin/$tool" >&2
        exit 77
      }
  done
  [ "$(env -i PATH="$TOOLCHAIN_ROOT/bin:/usr/bin:/bin" \
    "$CARGO_AUDIT_BIN" --version)" = "cargo-audit $CARGO_AUDIT_VERSION" ] || {
    echo "pinned cargo-audit must be exactly version $CARGO_AUDIT_VERSION" >&2
    exit 77
  }
  validate_root_directory /usr || exit 77
  validate_root_directory /usr/bin || exit 77
  [ -f /usr/bin/bwrap ] && [ ! -L /usr/bin/bwrap ] \
    && [ -x /usr/bin/bwrap ] && [ "$(stat -Lc %u -- /usr/bin/bwrap)" -eq 0 ] || {
      echo "root-owned bubblewrap is required at /usr/bin/bwrap" >&2
      exit 77
    }
  local bwrap_mode
  bwrap_mode=$(stat -Lc %a -- /usr/bin/bwrap)
  [ "$((8#$bwrap_mode & 8#022))" -eq 0 ] || {
    echo "/usr/bin/bwrap must not be group/world writable" >&2
    exit 77
  }
}

content_digest () {
  local root=$1
  [ -d "$root" ] && [ ! -L "$root" ] || return 1
  if find "$root" \! -type d \! -type f -print -quit | grep -q .; then
    echo "content tree contains a link or special file: $root" >&2
    return 1
  fi
  if find "$root" -type f -links +1 -print -quit | grep -q .; then
    echo "content tree contains a hard-linked file: $root" >&2
    return 1
  fi
  (
    cd -- "$root"
    while IFS= read -r -d '' entry; do
      local mode
      mode=$(stat -Lc %a -- "$entry") || exit 1
      if [ -d "$entry" ]; then
        printf 'directory\0%s\0%s\0' "${entry#./}" "$mode"
      elif [ -f "$entry" ]; then
        local file_hash
        file_hash=$(sha256sum -- "$entry" | awk '{print $1}') || exit 1
        printf 'file\0%s\0%s\0%s\0' "${entry#./}" "$mode" "$file_hash"
      else
        exit 1
      fi
    done < <(find . -mindepth 1 -print0 | LC_ALL=C sort -z)
  ) | sha256sum | awk '{print $1}'
}

validate_lockfile_sources () {
  local lockfile=$1
  [ -f "$lockfile" ] && [ ! -L "$lockfile" ] || {
    echo "release lockfile is missing or is not a regular file" >&2
    return 1
  }
  /usr/bin/python3 -I - "$lockfile" <<'PY'
import os
import stat
import sys
import tomllib

approved_source = "registry+https://github.com/rust-lang/crates.io-index"
path = sys.argv[1]
fd = -1
try:
    fd = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    metadata = os.fstat(fd)
    if not stat.S_ISREG(metadata.st_mode):
        raise ValueError("not a regular file")
    with os.fdopen(fd, "rb", closefd=True) as handle:
        fd = -1
        document = tomllib.load(handle)
    if not isinstance(document, dict):
        raise ValueError("invalid lockfile root")
    packages = document.get("package")
    if not isinstance(packages, list) or not packages:
        raise ValueError("missing package array")
    for package in packages:
        if not isinstance(package, dict):
            raise ValueError("invalid package entry")
        if "source" not in package:
            continue
        source = package["source"]
        if not isinstance(source, str) or source != approved_source:
            raise ValueError("unapproved package source")
except (OSError, ValueError, tomllib.TOMLDecodeError):
    raise SystemExit(1)
finally:
    if fd >= 0:
        os.close(fd)
PY
}

content_approval_value () {
  local key=$1
  awk -F= -v wanted="$key" '
    $1 == wanted { value = substr($0, length($1) + 2); count++ }
    END { if (count == 1) print value; else exit 1 }
  ' "$CONTENT_APPROVAL"
}

service_generation () {
  local boot_id invocation started
  boot_id=$(tr -d '\n' </proc/sys/kernel/random/boot_id) || return 1
  invocation=$(systemctl show "$SERVICE" --property=InvocationID --value) || return 1
  started=$(systemctl show "$SERVICE" --property=ExecMainStartTimestampMonotonic --value) || return 1
  [ -n "$invocation" ] || invocation=none
  [ -n "$started" ] || started=0
  [[ "$boot_id" =~ ^[0-9a-f-]{36}$ ]] \
    && { [ "$invocation" = none ] || [[ "$invocation" =~ ^[0-9a-f]{32}$ ]]; } \
    && [[ "$started" =~ ^[0-9]+$ ]] || return 1
  printf '%s|%s|%s' "$boot_id" "$invocation" "$started"
}

require_content_approval () {
  local release=$1
  local expected approved
  CONTENT_APPROVAL_MANIFEST=none
  expected=$(cat -- "$release/CONTENT_SHA256") || return 1
  [ -f "$CONTENT_APPROVAL" ] && [ ! -L "$CONTENT_APPROVAL" ] \
    && [ "$(stat -Lc %u -- "$CONTENT_APPROVAL")" -eq 0 ] || {
      echo "release content is not approved for the mutable runtime tree" >&2
      echo "run the documented offline content reconciliation and content-approve workflow" >&2
      return 1
    }
  local approval_mode
  approval_mode=$(stat -Lc %a -- "$CONTENT_APPROVAL")
  [ "$((8#$approval_mode & 8#022))" -eq 0 ] || {
    echo "content approval marker is group/world writable" >&2
    return 1
  }
  LC_ALL=C awk -F= '
    $1 == "format" || $1 == "content_sha256" || $1 == "revision" \
      || $1 == "backup_manifest" || $1 == "database_sha256" \
      || $1 == "lib_sha256" || $1 == "live_digest" || $1 == "approved_epoch" \
      || $1 == "boot_id" || $1 == "service_invocation_id" \
      || $1 == "service_start_monotonic" { seen[$1]++; next }
    { bad = 1 }
    END {
      required[1] = "format"; required[2] = "content_sha256"; required[3] = "revision"
      required[4] = "backup_manifest"; required[5] = "database_sha256"
      required[6] = "lib_sha256"; required[7] = "live_digest"
      required[8] = "approved_epoch"; required[9] = "boot_id"
      required[10] = "service_invocation_id"; required[11] = "service_start_monotonic"
      for (i = 1; i <= 11; i++) if (seen[required[i]] != 1) bad = 1
      exit bad ? 1 : 0
    }
  ' "$CONTENT_APPROVAL" || {
    echo "content approval marker has an invalid field set" >&2
    return 1
  }
  [ "$(content_approval_value format)" = deltamud-content-approval-v1 ] || return 1
  approved=$(content_approval_value content_sha256) || return 1
  [ "$approved" = "$expected" ] || {
    echo "release content digest $expected has not been approved (current: ${approved:-none})" >&2
    return 1
  }
  local revision manifest database_hash lib_hash live_digest approved_epoch
  local boot_id invocation started
  revision=$(content_approval_value revision) || return 1
  manifest=$(content_approval_value backup_manifest) || return 1
  database_hash=$(content_approval_value database_sha256) || return 1
  lib_hash=$(content_approval_value lib_sha256) || return 1
  live_digest=$(content_approval_value live_digest) || return 1
  approved_epoch=$(content_approval_value approved_epoch) || return 1
  boot_id=$(content_approval_value boot_id) || return 1
  invocation=$(content_approval_value service_invocation_id) || return 1
  started=$(content_approval_value service_start_monotonic) || return 1
  [[ "$revision" =~ ^[0-9a-f]{40}$ ]] \
    && [[ "$database_hash" =~ ^[0-9a-f]{64}$ ]] \
    && [[ "$lib_hash" =~ ^[0-9a-f]{64}$ ]] \
    && [[ "$live_digest" =~ ^[0-9a-f]{64}$ ]] \
    && [[ "$approved_epoch" =~ ^[0-9]+$ ]] \
    && [[ "$boot_id" =~ ^[0-9a-f-]{36}$ ]] \
    && { [ "$invocation" = none ] || [[ "$invocation" =~ ^[0-9a-f]{32}$ ]]; } \
    && [[ "$started" =~ ^[0-9]+$ ]] || {
      echo "content approval marker has invalid values" >&2
      return 1
    }
  case "$manifest" in
    "$BACKUP_ROOT"/backup.*/manifest) ;;
    *) echo "content approval backup is outside the fixed backup root" >&2; return 1 ;;
  esac
  CONTENT_APPROVAL_MANIFEST=$manifest
}

validate_content_rollback_approval () {
  local release=$1
  require_content_approval "$release" || return 1
  local revision=${release##*/}
  [ "$(content_approval_value revision)" = "$revision" ] || {
    echo "content transition requires approval for the exact target revision" >&2
    return 1
  }
  validate_backup_manifest "$CONTENT_APPROVAL_MANIFEST" "$revision" any || return 1
  verify_service_stopped || {
    echo "content transition requires the service and runtime UID to remain fully stopped" >&2
    return 1
  }
  [ "$(content_digest /var/lib/deltamud/lib)" = "$(content_approval_value live_digest)" ] || {
    echo "runtime lib changed after content approval; repeat the stopped reconciliation and approval" >&2
    return 1
  }
  verify_database_identity || return 1
  [ "$DATABASE_IDENTITY_HASH" = "$(backup_manifest_value "$CONTENT_APPROVAL_MANIFEST" database_identity_sha256)" ] \
    && [ "$DATABASE_ENV_HASH" = "$(backup_manifest_value "$CONTENT_APPROVAL_MANIFEST" database_env_sha256)" ] || {
      echo "content rollback backup no longer matches the runtime database identity" >&2
      return 1
    }
  local approved_generation
  approved_generation="$(content_approval_value boot_id)|$(content_approval_value service_invocation_id)|$(content_approval_value service_start_monotonic)"
  [ "$approved_generation" = "$(service_generation)" ] || {
    echo "the service generation changed after content approval; repeat the stopped backup/reconciliation workflow" >&2
    return 1
  }
}

verify_service_stopped () {
  local first second active main_pid job runtime_pids
  stopped_snapshot () {
    local snapshot_active snapshot_pid snapshot_job service_uid effective real rc pids
    snapshot_active=$(systemctl show "$SERVICE" --property=ActiveState --value) || return 1
    snapshot_pid=$(systemctl show "$SERVICE" --property=MainPID --value) || return 1
    snapshot_job=$(systemctl show "$SERVICE" --property=Job --value) || return 1
    service_uid=$(id -u deltamud) || return 1
    if effective=$(pgrep -u "$service_uid" 2>/dev/null); then
      :
    else
      rc=$?
      [ "$rc" -eq 1 ] || return "$rc"
      effective=
    fi
    if real=$(pgrep -U "$service_uid" 2>/dev/null); then
      :
    else
      rc=$?
      [ "$rc" -eq 1 ] || return "$rc"
      real=
    fi
    pids=$({ [ -z "$effective" ] || printf '%s\n' "$effective"; \
      [ -z "$real" ] || printf '%s\n' "$real"; } | sort -un | paste -sd, -) || return 1
    printf '%s|%s|%s|%s' "$snapshot_active" "$snapshot_pid" "$snapshot_job" "$pids"
  }
  first=$(stopped_snapshot) || return 1
  sleep 0.1 || true
  second=$(stopped_snapshot) || return 1
  [ "$first" = "$second" ] || return 1
  IFS='|' read -r active main_pid job runtime_pids <<<"$second"
  [ "$active" = inactive ] && [ "$main_pid" = 0 ] \
    && [ -z "$job" ] && [ -z "$runtime_pids" ]
}

queue_systemd_job () {
  # Critical-section signals are recorded by the parent. Ignore them in the
  # short-lived systemctl client so a queued job is not mistaken for a canceled
  # job when the waiter alone receives a terminal signal.
  (
    trap '' HUP INT TERM
    exec systemctl --no-block "$@"
  )
}

migration_unit_snapshot () {
  local unit=$1
  local active pid job invocation result code status started
  active=$(systemctl show "$unit" --property=ActiveState --value) || return 1
  pid=$(systemctl show "$unit" --property=MainPID --value) || return 1
  job=$(systemctl show "$unit" --property=Job --value) || return 1
  invocation=$(systemctl show "$unit" --property=InvocationID --value) || return 1
  result=$(systemctl show "$unit" --property=Result --value) || return 1
  code=$(systemctl show "$unit" --property=ExecMainCode --value) || return 1
  status=$(systemctl show "$unit" --property=ExecMainStatus --value) || return 1
  started=$(systemctl show "$unit" --property=ExecMainStartTimestampMonotonic --value) || return 1
  printf '%s|%s|%s|%s|%s|%s|%s|%s' \
    "$active" "$pid" "$job" "$invocation" "$result" "$code" "$status" "$started"
}

stable_terminal_migration_snapshot () {
  local unit=$1
  local first second active pid job invocation result code status started
  first=$(migration_unit_snapshot "$unit") || return 1
  sleep 0.2 || true
  second=$(migration_unit_snapshot "$unit") || return 1
  [ "$first" = "$second" ] || return 1
  IFS='|' read -r active pid job invocation result code status started <<<"$second"
  { [ "$active" = inactive ] || [ "$active" = failed ]; } \
    && [ "$pid" = 0 ] && [ -z "$job" ] || return 1
  printf '%s' "$second"
}

migration_marker_matches () {
  local expected_attempt=$1
  local expected_target=$2
  local expected_backup=$3
  local expected_content=$4
  validate_activation_marker_file \
    && [ "$(activation_marker_value state)" = migrating ] \
    && [ "$(activation_marker_value attempt)" = "$expected_attempt" ] \
    && [ "$(activation_marker_value target)" = "$expected_target" ] \
    && [ "$(activation_marker_value backup_manifest)" = "$expected_backup" ] \
    && [ "$(activation_marker_value content_rollback_manifest)" = "$expected_content" ]
}

wait_service_stopped () {
  local attempt
  for ((attempt = 0; attempt < 250; attempt++)); do
    verify_service_stopped && return 0
    sleep 0.2 || true
  done
  return 1
}

stop_service_and_wait () {
  local queue_rc=0
  queue_systemd_job stop "$SERVICE" || queue_rc=$?
  if wait_service_stopped; then
    return 0
  fi
  echo "$SERVICE did not reach a verified inactive/MainPID=0 state (queue status $queue_rc)" >&2
  return 1
}

start_service_and_wait () {
  local expected_binary=$1
  local queue_rc=0
  queue_systemd_job start "$SERVICE" || queue_rc=$?
  [ "$queue_rc" -eq 0 ] || {
    echo "could not queue $SERVICE start (status $queue_rc)" >&2
    return 1
  }
  if wait_ready "$expected_binary"; then
    return 0
  fi
  echo "$SERVICE did not complete its guarded start (queue status $queue_rc)" >&2
  return 1
}

stop_service_for_offline_work () {
  validate_main_unit || return 1
  if ! stop_service_and_wait; then
    echo "$SERVICE refused or failed its OLC-safe stop" >&2
    return 1
  fi
}

normalize_database_endpoint () {
  [ "$#" -eq 2 ] || return 1
  DATABASE_ENDPOINT_HOST_VALUE=$1 DATABASE_ENDPOINT_PORT_VALUE=$2 \
    /usr/bin/python3 -I - <<'PY'
import ipaddress
import os
import re
import sys

try:
    host = os.environ["DATABASE_ENDPOINT_HOST_VALUE"]
    port_text = os.environ["DATABASE_ENDPOINT_PORT_VALUE"]
    if not host or host != host.strip() or "%" in host:
        raise ValueError("invalid host")
    if any(ord(ch) < 32 or 127 <= ord(ch) <= 159 for ch in host):
        raise ValueError("invalid host")
    if not re.fullmatch(r"[0-9]+", port_text) or len(port_text) > 10:
        raise ValueError("invalid port")
    port = int(port_text, 10)
    if not 1 <= port <= 65535:
        raise ValueError("invalid port")

    try:
        normalized_host = ipaddress.ip_address(host).compressed.lower()
    except ValueError:
        if ":" in host:
            raise
        if host.endswith("."):
            host = host[:-1]
        host.encode("ascii")
        normalized_host = host.lower()
        if not normalized_host or len(normalized_host) > 253:
            raise ValueError("invalid DNS host")
        labels = normalized_host.split(".")
        label_pattern = re.compile(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?")
        if any(not label_pattern.fullmatch(label) for label in labels):
            raise ValueError("invalid DNS host")
        if re.fullmatch(r"[0-9.]+", normalized_host):
            raise ValueError("ambiguous numeric host")

    print(normalized_host)
    print(port)
except Exception:
    sys.exit(1)
PY
}

parse_backup_config_endpoint () {
  local config=${1:-$BACKUP_CNF}
  BACKUP_ENDPOINT_HOST=
  BACKUP_ENDPOINT_PORT=
  [ -f "$config" ] && [ ! -L "$config" ] || return 1

  local raw_output normalized_output
  local raw_endpoint=() normalized_endpoint=()
  if ! raw_output=$(/usr/bin/python3 -I - "$config" <<'PY'
import pathlib
import sys

try:
    data = pathlib.Path(sys.argv[1]).read_bytes()
    if b"\x00" in data:
        raise ValueError("NUL is forbidden")
    text = data.decode("utf-8")
    allowed = {"protocol", "host", "port", "user", "password"}
    seen = {}
    values = {}
    sections = 0
    in_client = False
    for raw_line in text.splitlines():
        stripped = raw_line.strip(" \t\r")
        if not stripped or stripped.startswith(("#", ";")):
            continue
        if stripped == "[client]":
            sections += 1
            in_client = True
            continue
        if not in_client or "=" not in raw_line:
            raise ValueError("entry outside client group")
        key, value = raw_line.split("=", 1)
        key = key.strip(" \t")
        value = value.strip(" \t\r")
        if key not in allowed or not value:
            raise ValueError("invalid client entry")
        if any(ord(ch) < 32 or 127 <= ord(ch) <= 159 for ch in value):
            raise ValueError("control character in client entry")
        seen[key] = seen.get(key, 0) + 1
        values[key] = value
    if sections != 1 or set(seen) != allowed:
        raise ValueError("incomplete client group")
    if any(count != 1 for count in seen.values()):
        raise ValueError("duplicate client entry")
    if values["protocol"] != "tcp":
        raise ValueError("backup protocol must be tcp")
    print(values["host"])
    print(values["port"])
except Exception:
    sys.exit(1)
PY
  ); then
    return 1
  fi
  mapfile -t raw_endpoint <<<"$raw_output"
  [ "${#raw_endpoint[@]}" -eq 2 ] || return 1
  if ! normalized_output=$(normalize_database_endpoint \
    "${raw_endpoint[0]}" "${raw_endpoint[1]}"); then
    return 1
  fi
  mapfile -t normalized_endpoint <<<"$normalized_output"
  [ "${#normalized_endpoint[@]}" -eq 2 ] || return 1
  BACKUP_ENDPOINT_HOST=${normalized_endpoint[0]}
  BACKUP_ENDPOINT_PORT=${normalized_endpoint[1]}
}

database_endpoints_match () {
  [ "$#" -eq 4 ] \
    && [ "$1" = "$3" ] \
    && [ "$2" = "$4" ]
}

validate_backup_config () {
  validate_root_directory /etc || return 1
  validate_root_directory /etc/deltamud || return 1
  [ -f "$BACKUP_CNF" ] && [ ! -L "$BACKUP_CNF" ] \
    && [ "$(stat -Lc %u -- "$BACKUP_CNF")" -eq 0 ] || {
      echo "database backup config must be a root-owned regular file: $BACKUP_CNF" >&2
      return 1
    }
  local mode
  mode=$(stat -Lc %a -- "$BACKUP_CNF")
  [ "$((8#$mode & 8#077))" -eq 0 ] || {
    echo "database backup config must be mode 0600 or narrower" >&2
    return 1
  }
  parse_backup_config_endpoint "$BACKUP_CNF" || {
    echo "$BACKUP_CNF must contain one strict [client] group with protocol=tcp and unique, valid host/port/user/password entries" >&2
    return 1
  }
}

validate_runtime_environment () {
  validate_root_directory /etc || return 1
  validate_root_directory /etc/deltamud || return 1
  [ -f "$DATABASE_ENV" ] && [ ! -L "$DATABASE_ENV" ] \
    && [ "$(stat -Lc %u -- "$DATABASE_ENV")" -eq 0 ] || {
      echo "runtime environment must be a root-owned regular file" >&2
      return 1
    }
  local env_mode
  env_mode=$(stat -Lc %a -- "$DATABASE_ENV")
  [ "$env_mode" = 640 ] && [ "$(stat -Lc %g -- "$DATABASE_ENV")" -eq "$(id -g deltamud)" ] || {
    echo "runtime environment must be root:deltamud mode 0640" >&2
    return 1
  }
  LC_ALL=C awk -F= '
    /^[[:space:]]*([#;]|$)/ { next }
    /^[A-Z_][A-Z0-9_]*=[^[:space:]]+$/ {
      key = $1
      value = substr($0, length(key) + 2)
      seen[key]++
      values[key] = value
      if (value ~ /["\\]/ || index(value, sprintf("%c", 39)) != 0) bad = 1
      if (key ~ /^(LD_|BASH_ENV$|ENV$|SHELLOPTS$|BASHOPTS$|CDPATH$|GLOBIGNORE$|PYTHONPATH$|PYTHONHOME$|PERL5LIB$|PERLLIB$|RUBYLIB$|GEM_HOME$|GEM_PATH$|GCONV_PATH$|GETCONF_DIR$)/) bad = 1
      next
    }
    { bad = 1 }
    END {
      for (key in seen) if (seen[key] != 1) bad = 1
      if (seen["DATABASE_URL"] != 1) bad = 1
      if (seen["MUD_LIB_PATH"] != 1 || values["MUD_LIB_PATH"] != "/var/lib/deltamud/lib") bad = 1
      if (seen["MUD_MOCK_DB"] != 1 || values["MUD_MOCK_DB"] != "false") bad = 1
      if (seen["MUD_EXEC_PATH"] != 1 || values["MUD_EXEC_PATH"] != "/opt/deltamud/current/bin/deltamud") bad = 1
      if (seen["MUD_METRICS_BIND"] != 1 || values["MUD_METRICS_BIND"] != "127.0.0.1") bad = 1
      if (seen["MUD_METRICS_PORT"] != 1 || values["MUD_METRICS_PORT"] != "19595") bad = 1
      exit bad ? 1 : 0
    }
  ' "$DATABASE_ENV" || {
    echo "$DATABASE_ENV must use unique literal assignments without whitespace, quotes, or backslashes and the canonical runtime/lib/copyover/readiness values" >&2
    return 1
  }
}

load_runtime_environment () {
  validate_runtime_environment || return 1
  RUNTIME_ENV_ARGS=()
  local line key value
  while IFS= read -r line || [ -n "$line" ]; do
    [[ "$line" =~ ^[[:space:]]*([#\;]|$) ]] && continue
    key=${line%%=*}
    value=${line#*=}
    RUNTIME_ENV_ARGS+=("$key=$value")
  done <"$DATABASE_ENV"
}

run_installed_maintenance () {
  local release=$1
  shift
  load_runtime_environment || return 1
  local service_uid service_gid
  service_uid=$(id -u deltamud) || return 1
  service_gid=$(id -g deltamud) || return 1
  (
    trap '' HUP INT TERM
    cd /var/lib/deltamud || exit 1
    exec setpriv --reuid="$service_uid" --regid="$service_gid" --clear-groups \
      --no-new-privs --bounding-set=-all --inh-caps=-all --ambient-caps=-all \
      --pdeathsig SIGKILL env -i PATH=/usr/bin:/bin LANG=C.UTF-8 TZ=UTC \
      "${RUNTIME_ENV_ARGS[@]}" "$release/bin/deltamud" "$@"
  ) 9>&-
}

parse_database_url_identity () {
  [ "$#" -eq 1 ] || return 1
  DATABASE_URL_VALUE=$1 /usr/bin/python3 -I -c '
import base64, os, sys
from urllib.parse import unquote, urlsplit
try:
    value = os.environ["DATABASE_URL_VALUE"]
    parsed = urlsplit(value)
    user = unquote(parsed.username or "")
    password = unquote(parsed.password or "")
    host = parsed.hostname or ""
    port = str(3306 if parsed.port is None else parsed.port)
    database = parsed.path.removeprefix("/")
    fields = (user, password, host, port, database)
    if parsed.scheme != "mysql" or parsed.query or parsed.fragment:
        raise ValueError("unsupported database URL form")
    if not user or not password or not host or database != "deltamud":
        raise ValueError("incomplete database URL identity")
    if any(any(ord(ch) < 32 or ord(ch) == 127 for ch in field) for field in fields):
        raise ValueError("control characters are forbidden")
    for field in fields:
        print(base64.b64encode(field.encode()).decode())
except Exception:
    sys.exit(1)
'
}

verify_database_identity () {
  validate_backup_config || return 1
  validate_runtime_environment || return 1
  local database_lines=()
  mapfile -t database_lines < <(awk '/^DATABASE_URL=/ { print substr($0, 14) }' "$DATABASE_ENV")
  [ "${#database_lines[@]}" -eq 1 ] && [[ "${database_lines[0]}" != *[[:space:]]* ]] || {
    echo "$DATABASE_ENV must contain exactly one unquoted, whitespace-free DATABASE_URL" >&2
    return 1
  }
  local parsed=()
  mapfile -t parsed < <(parse_database_url_identity "${database_lines[0]}")
  [ "${#parsed[@]}" -eq 5 ] || {
    echo "could not parse the runtime DATABASE_URL identity" >&2
    return 1
  }
  local db_user db_password db_host db_port db_name
  db_user=$(printf '%s' "${parsed[0]}" | base64 -d)
  db_password=$(printf '%s' "${parsed[1]}" | base64 -d)
  db_host=$(printf '%s' "${parsed[2]}" | base64 -d)
  db_port=$(printf '%s' "${parsed[3]}" | base64 -d)
  db_name=$(printf '%s' "${parsed[4]}" | base64 -d)
  local normalized_runtime_output
  local normalized_runtime_endpoint=()
  if ! normalized_runtime_output=$(normalize_database_endpoint "$db_host" "$db_port"); then
    echo "runtime database URL has an invalid host or port" >&2
    return 1
  fi
  mapfile -t normalized_runtime_endpoint <<<"$normalized_runtime_output"
  [ "${#normalized_runtime_endpoint[@]}" -eq 2 ] || {
    echo "could not normalize the runtime database endpoint" >&2
    return 1
  }
  local normalized_runtime_host=${normalized_runtime_endpoint[0]}
  local normalized_runtime_port=${normalized_runtime_endpoint[1]}
  database_endpoints_match "$BACKUP_ENDPOINT_HOST" "$BACKUP_ENDPOINT_PORT" \
    "$normalized_runtime_host" "$normalized_runtime_port" || {
      echo "backup credential host and port do not exactly match the normalized runtime DATABASE_URL endpoint" >&2
      unset db_password
      return 1
    }
  local query runtime_identity backup_identity
  query="SELECT CONCAT(@@server_id, CHAR(9), @@hostname, CHAR(9), @@port, CHAR(9), DATABASE())"
  runtime_identity=$(MYSQL_PWD="$db_password" mariadb --no-defaults --protocol=tcp \
    --connect-timeout=5 --batch --skip-column-names \
    --host="$db_host" --port="$db_port" --user="$db_user" --database="$db_name" \
    -e "$query") || {
      echo "could not query the runtime DATABASE_URL identity" >&2
      return 1
    }
  backup_identity=$(mariadb --defaults-file="$BACKUP_CNF" --connect-timeout=5 \
    --batch --skip-column-names --database=deltamud -e "$query") || {
      echo "could not query the backup credential database identity" >&2
      return 1
    }
  [ -n "$runtime_identity" ] && [ "$runtime_identity" = "$backup_identity" ] || {
    echo "backup credential and runtime DATABASE_URL resolve to different databases" >&2
    return 1
  }
  if ! DATABASE_IDENTITY_HASH=$(printf '%s' "$runtime_identity" | sha256sum | awk '{print $1}') \
    || ! DATABASE_ENV_HASH=$(sha256sum -- "$DATABASE_ENV" | awk '{print $1}'); then
    echo "could not bind the verified database identity to its environment" >&2
    return 1
  fi
  unset db_password database_lines parsed runtime_identity backup_identity
}

database_object_inventory () {
  local schema=$1
  [ "$schema" = deltamud ] \
    || [[ "$schema" =~ ^deltamud_restorecheck_[0-9]+_[0-9]+$ ]] || {
      echo "refusing inventory of unexpected database: $schema" >&2
      return 1
    }
  mariadb --defaults-file="$BACKUP_CNF" --batch --skip-column-names --raw \
    -e "SELECT object_kind, object_name, object_detail FROM (
      SELECT 'table' AS object_kind, HEX(TABLE_NAME) AS object_name,
             HEX(TABLE_TYPE) AS object_detail
        FROM information_schema.tables WHERE table_schema='$schema'
      UNION ALL
      SELECT 'routine', HEX(ROUTINE_NAME), HEX(ROUTINE_TYPE)
        FROM information_schema.routines WHERE routine_schema='$schema'
      UNION ALL
      SELECT 'trigger', HEX(TRIGGER_NAME),
             HEX(CONCAT(EVENT_OBJECT_TABLE, ':', ACTION_TIMING, ':', EVENT_MANIPULATION))
        FROM information_schema.triggers WHERE trigger_schema='$schema'
      UNION ALL
      SELECT 'event', HEX(EVENT_NAME), HEX(STATUS)
        FROM information_schema.events WHERE event_schema='$schema'
    ) AS inventory ORDER BY object_kind, object_name, object_detail"
}

create_release_backup () {
  local revision=$1
  local backup_kind=${2:-stateful}
  case "$backup_kind" in
    stateful|empty-initialization) ;;
    *) echo "invalid release-backup kind: $backup_kind" >&2; return 1 ;;
  esac
  verify_service_stopped || {
    echo "release backup requires $SERVICE to be fully stopped" >&2
    return 1
  }
  validate_backup_config || return 1
  verify_database_identity || return 1
  for command in mariadb mariadb-dump tar sha256sum; do
    command -v "$command" >/dev/null 2>&1 || {
      echo "backup command is unavailable: $command" >&2
      return 1
    }
  done
  validate_root_directory /var || return 1
  validate_root_directory /var/backups || return 1
  if ! install -d -o root -g root -m 0700 "$BACKUP_ROOT"; then
    echo "could not create the fixed backup root" >&2
    return 1
  fi
  validate_root_directory "$BACKUP_ROOT" || return 1
  [ -d /var/lib/deltamud/lib ] && [ ! -L /var/lib/deltamud/lib ] || {
    echo "runtime lib is missing or linked: /var/lib/deltamud/lib" >&2
    return 1
  }

  local database_exists
  if ! database_exists=$(mariadb --defaults-file="$BACKUP_CNF" --batch --skip-column-names \
    -e "SELECT COUNT(*) FROM information_schema.schemata WHERE schema_name='deltamud'"); then
    echo "backup credential could not query the database inventory" >&2
    return 1
  fi
  [ "$database_exists" = 1 ] || {
    echo "backup credential cannot identify the deltamud database" >&2
    return 1
  }
  local source_inventory source_inventory_hash source_objects
  if ! source_inventory=$(database_object_inventory deltamud); then
    echo "could not inventory database objects before backup" >&2
    return 1
  fi
  source_objects=$(printf '%s' "$source_inventory" | awk 'NF { count++ } END { print count + 0 }')
  source_inventory_hash=$(printf '%s' "$source_inventory" | sha256sum | awk '{print $1}') || return 1
  if [ "$backup_kind" = empty-initialization ] && [ "$source_objects" != 0 ]; then
    echo "initialization requires an existing deltamud database with zero objects" >&2
    return 1
  fi

  local created directory_name database_dump lib_archive
  if ! created=$(date +%s); then
    echo "could not timestamp the backup" >&2
    return 1
  fi
  directory_name="backup.${created}.${revision}"
  if ! BACKUP_STAGE=$(mktemp -d "$BACKUP_ROOT/${directory_name}.XXXXXX"); then
    BACKUP_STAGE=
    echo "could not create the backup stage" >&2
    return 1
  fi
  case "$BACKUP_STAGE" in
    "$BACKUP_ROOT"/backup.*) ;;
    *) echo "backup stage escaped the fixed root" >&2; return 1 ;;
  esac
  [ -d "$BACKUP_STAGE" ] && [ ! -L "$BACKUP_STAGE" ] || return 1
  chmod 0700 "$BACKUP_STAGE" || return 1
  database_dump="$BACKUP_STAGE/database.sql"
  lib_archive="$BACKUP_STAGE/lib.tar"
  if ! mariadb-dump --defaults-file="$BACKUP_CNF" \
    --single-transaction --quick --hex-blob --triggers --routines --events \
    --default-character-set=utf8mb4 deltamud >"$database_dump"; then
    echo "database backup failed" >&2
    return 1
  fi
  [ -s "$database_dump" ] || {
    echo "database backup is empty" >&2
    return 1
  }

  RESTORE_CHECK_SCHEMA="deltamud_restorecheck_${created}_$$"
  if ! mariadb --defaults-file="$BACKUP_CNF" \
    -e "CREATE DATABASE \`$RESTORE_CHECK_SCHEMA\` CHARACTER SET utf8mb4"; then
    echo "could not create the isolated restore-drill database" >&2
    return 1
  fi
  if ! mariadb --defaults-file="$BACKUP_CNF" "$RESTORE_CHECK_SCHEMA" \
    <"$database_dump"; then
    echo "database restore drill failed" >&2
    return 1
  fi
  local restored_inventory restored_objects
  if ! restored_inventory=$(database_object_inventory "$RESTORE_CHECK_SCHEMA"); then
    echo "could not verify the restored object inventory" >&2
    return 1
  fi
  restored_objects=$(printf '%s' "$restored_inventory" | awk 'NF { count++ } END { print count + 0 }')
  local restore_result=passed
  if [ "$backup_kind" = stateful ]; then
    [[ "$source_objects" =~ ^[1-9][0-9]*$ ]] \
      && [ "$restored_inventory" = "$source_inventory" ] || {
      echo "stateful restore typed object inventory differs from the source ($restored_objects objects restored; expected $source_objects)" >&2
      return 1
    }
  else
    [ "$restored_objects" = 0 ] && [ -z "$restored_inventory" ] || {
      echo "empty-database restore drill unexpectedly produced objects" >&2
      return 1
    }
    restore_result=passed-empty
  fi
  if ! mariadb --defaults-file="$BACKUP_CNF" \
    -e "DROP DATABASE \`$RESTORE_CHECK_SCHEMA\`"; then
    echo "restore drill passed, but its database could not be dropped: $RESTORE_CHECK_SCHEMA" >&2
    echo "the backup is rejected; remove that exact restore-check database after investigation" >&2
    return 1
  fi
  RESTORE_CHECK_SCHEMA=

  if ! tar --numeric-owner --acls --xattrs -C /var/lib/deltamud \
    -cf "$lib_archive" lib \
    || ! tar -tf "$lib_archive" >/dev/null \
    || ! chmod 0600 "$database_dump" "$lib_archive"; then
    echo "runtime-lib backup creation or verification failed" >&2
    return 1
  fi
  local database_hash lib_hash
  if ! database_hash=$(sha256sum -- "$database_dump" | awk '{print $1}') \
    || ! lib_hash=$(sha256sum -- "$lib_archive" | awk '{print $1}'); then
    echo "could not hash the backup artifacts" >&2
    return 1
  fi
  BACKUP_MANIFEST="$BACKUP_STAGE/manifest"
  if ! printf '%s\n' \
    'format=deltamud-backup-v1' \
    "revision=$revision" \
    "created_epoch=$created" \
    'database=deltamud' \
    "backup_kind=$backup_kind" \
    "database_identity_sha256=$DATABASE_IDENTITY_HASH" \
    "database_env_sha256=$DATABASE_ENV_HASH" \
    "object_inventory_sha256=$source_inventory_hash" \
    "database_dump=$database_dump" \
    "database_sha256=$database_hash" \
    "lib_archive=$lib_archive" \
    "lib_sha256=$lib_hash" \
    "restore_drill=$restore_result" >"$BACKUP_MANIFEST" \
    || ! chmod 0600 "$BACKUP_MANIFEST" \
    || ! sync -f "$database_dump" \
    || ! sync -f "$lib_archive" \
    || ! sync -f "$BACKUP_MANIFEST" \
    || ! sync -f "$BACKUP_STAGE" \
    || ! sync -f "$BACKUP_ROOT"; then
    echo "could not durably publish the verified backup manifest" >&2
    return 1
  fi
  BACKUP_STAGE=
  echo "verified release backup: $BACKUP_MANIFEST"
}

current_database_backup_kind () {
  verify_service_stopped || return 1
  validate_backup_config || return 1
  verify_database_identity || return 1
  local object_count
  object_count=$(mariadb --defaults-file="$BACKUP_CNF" --batch --skip-column-names \
    -e "SELECT (SELECT COUNT(*) FROM information_schema.tables WHERE table_schema='deltamud') + (SELECT COUNT(*) FROM information_schema.routines WHERE routine_schema='deltamud') + (SELECT COUNT(*) FROM information_schema.triggers WHERE trigger_schema='deltamud') + (SELECT COUNT(*) FROM information_schema.events WHERE event_schema='deltamud')") || return 1
  [[ "$object_count" =~ ^[0-9]+$ ]] || return 1
  if [ "$object_count" -eq 0 ]; then
    printf '%s\n' empty-initialization
  else
    printf '%s\n' stateful
  fi
}

backup_manifest_value () {
  local manifest=$1
  local key=$2
  awk -F= -v wanted="$key" '$1 == wanted { value = substr($0, length($1) + 2); count++ } END { if (count == 1) print value; else exit 1 }' \
    "$manifest"
}

validate_backup_manifest () {
  local manifest=$1
  local revision=$2
  local expected_kind=${3:-stateful}
  local freshness=${4:-fresh}
  case "$freshness" in
    fresh|historical) ;;
    *) echo "invalid backup-manifest freshness policy" >&2; return 1 ;;
  esac
  case "$manifest" in
    "$BACKUP_ROOT"/backup.*/manifest) ;;
    *) echo "backup manifest is outside the fixed backup root" >&2; return 1 ;;
  esac
  [ "$(readlink -f -- "$manifest" 2>/dev/null || true)" = "$manifest" ] \
    && [ -f "$manifest" ] && [ ! -L "$manifest" ] \
    && [ "$(stat -Lc %u -- "$manifest")" -eq 0 ] || {
      echo "backup manifest is missing, linked, or not root-owned" >&2
      return 1
    }
  local manifest_mode
  manifest_mode=$(stat -Lc %a -- "$manifest")
  [ "$((8#$manifest_mode & 8#077))" -eq 0 ] || {
    echo "backup manifest permissions are broader than 0600" >&2
    return 1
  }
  local backup_dir=${manifest%/manifest}
  validate_root_directory "$backup_dir" || return 1
  local backup_kind restore_result
  backup_kind=$(backup_manifest_value "$manifest" backup_kind) || return 1
  restore_result=$(backup_manifest_value "$manifest" restore_drill) || return 1
  case "$expected_kind:$backup_kind:$restore_result" in
    stateful:stateful:passed|empty-initialization:empty-initialization:passed-empty|any:stateful:passed|any:empty-initialization:passed-empty) ;;
    *) echo "backup manifest kind or restore result is invalid" >&2; return 1 ;;
  esac
  [ "$(backup_manifest_value "$manifest" format)" = deltamud-backup-v1 ] \
    && [ "$(backup_manifest_value "$manifest" revision)" = "$revision" ] \
    && [ "$(backup_manifest_value "$manifest" database)" = deltamud ] || {
      echo "backup manifest identity or restore verification is invalid" >&2
      return 1
    }
  local identity_hash env_hash inventory_hash
  identity_hash=$(backup_manifest_value "$manifest" database_identity_sha256)
  env_hash=$(backup_manifest_value "$manifest" database_env_sha256)
  inventory_hash=$(backup_manifest_value "$manifest" object_inventory_sha256 2>/dev/null || true)
  if [ "$freshness" = historical ] && [ -z "$inventory_hash" ]; then
    inventory_hash=legacy-unrecorded
  fi
  [[ "$identity_hash" =~ ^[0-9a-f]{64}$ ]] \
    && [[ "$env_hash" =~ ^[0-9a-f]{64}$ ]] \
    && { [[ "$inventory_hash" =~ ^[0-9a-f]{64}$ ]] \
      || [ "$inventory_hash" = legacy-unrecorded ]; } || {
    echo "backup manifest database identity binding is invalid" >&2
    return 1
  }
  local created now database_dump lib_archive database_hash lib_hash
  created=$(backup_manifest_value "$manifest" created_epoch)
  now=$(date +%s)
  [[ "$created" =~ ^[0-9]+$ ]] && [ "$created" -le "$now" ] || {
    echo "backup manifest timestamp is invalid" >&2
    return 1
  }
  if [ "$freshness" = fresh ] && [ "$((now - created))" -gt 86400 ]; then
    echo "backup manifest is not a fresh same-day backup" >&2
    return 1
  fi
  database_dump=$(backup_manifest_value "$manifest" database_dump)
  lib_archive=$(backup_manifest_value "$manifest" lib_archive)
  [ "$database_dump" = "$backup_dir/database.sql" ] \
    && [ "$lib_archive" = "$backup_dir/lib.tar" ] \
    && [ -s "$database_dump" ] && [ -f "$database_dump" ] && [ ! -L "$database_dump" ] \
    && [ -s "$lib_archive" ] && [ -f "$lib_archive" ] && [ ! -L "$lib_archive" ] || {
      echo "backup manifest artifact paths are invalid" >&2
      return 1
    }
  local artifact artifact_mode
  for artifact in "$database_dump" "$lib_archive"; do
    [ "$(stat -Lc %u -- "$artifact")" -eq 0 ] || {
      echo "backup artifact is not root-owned: $artifact" >&2
      return 1
    }
    artifact_mode=$(stat -Lc %a -- "$artifact")
    [ "$((8#$artifact_mode & 8#077))" -eq 0 ] || {
      echo "backup artifact permissions are broader than 0600: $artifact" >&2
      return 1
    }
  done
  database_hash=$(backup_manifest_value "$manifest" database_sha256)
  lib_hash=$(backup_manifest_value "$manifest" lib_sha256)
  [ "$database_hash" = "$(sha256sum -- "$database_dump" | awk '{print $1}')" ] \
    && [ "$lib_hash" = "$(sha256sum -- "$lib_archive" | awk '{print $1}')" ] || {
      echo "backup manifest artifact hash verification failed" >&2
      return 1
    }
}

select_build_identity () {
  BUILD_USER=deltamud-build
  if [ -z "$BUILD_USER" ] || [ "$BUILD_USER" = root ] || [ "$BUILD_USER" = deltamud ]; then
    echo "DELTAMUD_BUILD_USER must be a dedicated non-root account, not the runtime deltamud user" >&2
    exit 77
  fi
  id -u "$BUILD_USER" >/dev/null 2>&1 || {
    echo "release build user does not exist: $BUILD_USER" >&2
    exit 77
  }
  BUILD_UID=$(id -u "$BUILD_USER")
  [ "$BUILD_UID" -ne 0 ] || {
    echo "release build/test/canary execution is forbidden as root" >&2
    exit 77
  }
  if id -u deltamud >/dev/null 2>&1 && [ "$BUILD_UID" -eq "$(id -u deltamud)" ]; then
    echo "dedicated build user must not share the runtime service UID" >&2
    exit 77
  fi
  BUILD_GROUP=$(id -gn "$BUILD_USER")
  local build_gid build_gids private_gid_users group_members
  build_gid=$(id -g "$BUILD_USER")
  [ "$build_gid" -ne 0 ] || {
    echo "dedicated build user must not have root as its primary group" >&2
    exit 77
  }
  [ "$BUILD_GROUP" = "$BUILD_USER" ] \
    && [ "$(getent group "$BUILD_GROUP" | awk -F: '{print $3}')" = "$build_gid" ] || {
    echo "dedicated build user must have a same-named private primary group" >&2
    exit 77
  }
  group_members=$(getent group "$BUILD_GROUP" | awk -F: '{print $4}')
  [ -z "$group_members" ] || [ "$group_members" = "$BUILD_USER" ] || {
    echo "dedicated build group has additional explicit members" >&2
    exit 77
  }
  private_gid_users=$(getent passwd | awk -F: -v gid="$build_gid" '$4 == gid { print $1 }')
  [ "$private_gid_users" = "$BUILD_USER" ] || {
    echo "dedicated build group is a primary group for another account" >&2
    exit 77
  }
  build_gids=$(id -G "$BUILD_USER") || exit 77
  [ "$build_gids" = "$build_gid" ] || {
    echo "dedicated build user must have no supplementary group memberships" >&2
    exit 77
  }
  local account_status account_shell
  account_status=$(passwd -S "$BUILD_USER" 2>/dev/null | awk '{print $2}')
  case "$account_status" in
    L|LK) ;;
    *) echo "dedicated build account must have a locked password" >&2; exit 77 ;;
  esac
  account_shell=$(getent passwd "$BUILD_USER" | awk -F: '{print $7}')
  case "$account_shell" in
    */nologin|*/false) ;;
    *) echo "dedicated build account must use nologin or false" >&2; exit 77 ;;
  esac
  command -v pgrep >/dev/null 2>&1 || {
    echo "release build isolation requires pgrep" >&2
    exit 77
  }
  require_build_user_quiescent || exit 77
  command -v setpriv >/dev/null 2>&1 || {
    echo "release privilege drop requires setpriv" >&2
    exit 77
  }
  BUILD_WORK=$(mktemp -d /var/tmp/deltamud-release-build.XXXXXX)
  chmod 0755 "$BUILD_WORK"
  install -d -o "$BUILD_USER" -g "$BUILD_GROUP" -m 0700 \
    "$BUILD_WORK/home" "$BUILD_WORK/cargo-home" "$BUILD_WORK/git" \
    "$BUILD_WORK/target" "$BUILD_WORK/tmp"
  SOURCE_ROOT="$BUILD_WORK/source"
  SOURCE_MUD="$SOURCE_ROOT/rust-mud"
  install -d -o "$BUILD_USER" -g "$BUILD_GROUP" -m 0700 "$SOURCE_ROOT"
}

run_as_build_user () {
  local working_directory=$1
  shift
  case "$working_directory" in
    "$REPO_DIR"|"$BUILD_WORK"|"$SOURCE_ROOT"|"$SOURCE_ROOT"/*) ;;
    *) echo "refusing build command outside the checkout or source snapshot" >&2; return 77 ;;
  esac
  setpriv --reuid="$BUILD_USER" --regid="$BUILD_GROUP" --clear-groups \
    --no-new-privs --bounding-set=-all --inh-caps=-all --ambient-caps=-all \
    --pdeathsig SIGKILL env -i \
    HOME="$BUILD_WORK/home" USER="$BUILD_USER" LOGNAME="$BUILD_USER" SHELL=/bin/bash \
    PATH="$TOOLCHAIN_ROOT/bin:/usr/bin:/bin" \
    CARGO_HOME="$BUILD_WORK/cargo-home" \
    CARGO_TARGET_DIR="$BUILD_WORK/target" TMPDIR="$BUILD_WORK/tmp" \
    GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_ATTR_NOSYSTEM=1 \
    PYTHONDONTWRITEBYTECODE=1 LANG=C.UTF-8 TZ=UTC \
    /bin/bash -c 'cd -- "$1"; shift; exec "$@"' deltamud-build \
    "$working_directory" "$@" 9>&-
}

run_sandboxed_build () {
  local working_directory=$1
  shift
  case "$working_directory" in
    "$SOURCE_ROOT") local sandbox_work=/source ;;
    "$SOURCE_ROOT"/*) local sandbox_work="/source${working_directory#"$SOURCE_ROOT"}" ;;
    *) echo "refusing sandboxed build outside the frozen source snapshot" >&2; return 77 ;;
  esac
  setpriv --reuid="$BUILD_USER" --regid="$BUILD_GROUP" --clear-groups \
    --no-new-privs --bounding-set=-all --inh-caps=-all --ambient-caps=-all \
    --pdeathsig SIGKILL env -i \
    PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    /usr/bin/bwrap --unshare-all --unshare-user --new-session --die-with-parent --uid 1 --gid 1 \
    --cap-drop ALL --disable-userns --proc /proc --dev /dev --tmpfs /run \
    --ro-bind /usr /usr --symlink usr/bin /bin --symlink usr/sbin /sbin \
    --symlink usr/lib /lib --symlink usr/lib64 /lib64 --dir /etc \
    --ro-bind /etc/alternatives /etc/alternatives \
    --ro-bind /etc/ld.so.cache /etc/ld.so.cache \
    --ro-bind /etc/passwd /etc/passwd --ro-bind /etc/group /etc/group \
    --ro-bind /etc/nsswitch.conf /etc/nsswitch.conf \
    --dir /opt --dir /opt/deltamud --dir /opt/deltamud/toolchains \
    --ro-bind "$TOOLCHAIN_ROOT" "$TOOLCHAIN_ROOT" \
    --ro-bind "$SOURCE_ROOT" /source \
    --dir /work --bind "$BUILD_WORK/home" /work/home \
    --bind "$BUILD_WORK/cargo-home" /work/cargo-home \
    --bind "$BUILD_WORK/target" /work/target \
    --bind "$BUILD_WORK/tmp" /work/tmp --bind "$BUILD_WORK/tmp" /tmp \
    --dir /var --dir /var/tmp --chdir "$sandbox_work" --clearenv \
    --setenv PATH "$TOOLCHAIN_ROOT/bin:/usr/bin:/bin" \
    --setenv HOME /work/home --setenv USER "$BUILD_USER" \
    --setenv LOGNAME "$BUILD_USER" --setenv SHELL /bin/bash \
    --setenv CARGO_HOME /work/cargo-home --setenv CARGO_TARGET_DIR /work/target \
    --setenv TMPDIR /work/tmp --setenv CARGO_NET_OFFLINE true \
    --setenv GIT_CONFIG_NOSYSTEM 1 --setenv GIT_CONFIG_GLOBAL /dev/null \
    --setenv GIT_ATTR_NOSYSTEM 1 --setenv PYTHONDONTWRITEBYTECODE 1 \
    --setenv LANG C.UTF-8 --setenv LC_ALL C.UTF-8 --setenv TZ UTC \
    "$@" 9>&-
}

run_candidate_canary () {
  local staged_release=$1
  local artifacts=$2
  local canary_work="$BUILD_WORK/canary-work"
  local host_netns
  install -d -o "$BUILD_USER" -g "$BUILD_GROUP" -m 0700 \
    "$canary_work" "$canary_work/tmp"
  host_netns=$(readlink /proc/self/ns/net) || return 77
  setpriv --reuid="$BUILD_USER" --regid="$BUILD_GROUP" --clear-groups \
    --no-new-privs --bounding-set=-all --inh-caps=-all --ambient-caps=-all \
    --pdeathsig SIGKILL env -i \
    PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    HOME=/var/tmp TMPDIR=/var/tmp LANG=C.UTF-8 LC_ALL=C.UTF-8 TZ=UTC \
    timeout --foreground --signal=TERM --kill-after=15 270 \
    /usr/bin/bwrap --unshare-all --unshare-user --new-session --die-with-parent --uid 1 --gid 1 \
    --cap-drop ALL --disable-userns --proc /proc --dev /dev --tmpfs /run \
    --ro-bind /usr /usr --symlink usr/bin /bin --symlink usr/sbin /sbin \
    --symlink usr/lib /lib --symlink usr/lib64 /lib64 --dir /etc \
    --ro-bind /etc/alternatives /etc/alternatives \
    --ro-bind /etc/ld.so.cache /etc/ld.so.cache \
    --ro-bind /etc/passwd /etc/passwd --ro-bind /etc/group /etc/group \
    --ro-bind /etc/nsswitch.conf /etc/nsswitch.conf \
    --ro-bind /etc/hosts /etc/hosts --ro-bind /etc/resolv.conf /etc/resolv.conf \
    --ro-bind "$SOURCE_ROOT" /source --ro-bind "$staged_release" /input \
    --dir /work --bind "$canary_work" /work \
    --bind "$canary_work/tmp" /tmp --bind "$artifacts" /artifacts \
    --dir /var --dir /var/tmp --chdir /work --clearenv \
    --setenv PATH /usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    --setenv HOME /work --setenv TMPDIR /work/tmp --setenv LANG C.UTF-8 \
    --setenv LC_ALL C.UTF-8 --setenv TZ UTC --setenv PYTHONDONTWRITEBYTECODE 1 \
    --setenv DELTAMUD_CANARY_SANDBOXED 1 \
    --setenv CANARY_HOST_NETNS "$host_netns" \
    --setenv CANARY_HARD_TIMEOUT_SECONDS 270 --setenv CANARY_KILL_AFTER_SECONDS 15 \
    --setenv RUST_BIN /input/bin/deltamud \
    --setenv MUD_CANARY_SOURCE_LIB /input/content/lib \
    /bin/bash /source/rust-mud/scripts/canary.sh \
      --seconds 90 --players 3 --artifacts /artifacts 9>&-
}

require_root_paths () {
  [ "$RELEASE_ROOT" = /opt/deltamud/releases ] || {
    echo "refusing unexpected release root: $RELEASE_ROOT" >&2
    exit 64
  }
  [ "$CURRENT_LINK" = /opt/deltamud/current ] || {
    echo "refusing unexpected current link: $CURRENT_LINK" >&2
    exit 64
  }
  [ "$PREVIOUS_LINK" = /opt/deltamud/previous ] || {
    echo "refusing unexpected previous link: $PREVIOUS_LINK" >&2
    exit 64
  }
  [ "$(id -u)" -eq 0 ] || {
    echo "release activation must run as root" >&2
    exit 77
  }
  validate_root_directory /opt || exit 77
  if [ -e /opt/deltamud ] || [ -L /opt/deltamud ]; then
    validate_root_directory /opt/deltamud || exit 77
  fi
  if [ -e "$RELEASE_ROOT" ] || [ -L "$RELEASE_ROOT" ]; then
    validate_root_directory "$RELEASE_ROOT" || exit 77
  fi
  ensure_activation_root || exit 77
}

wait_ready () {
  local expected_binary
  expected_binary=$(readlink -f -- "$1") || return 1
  local attempts=100
  local attempt
  for ((attempt = 0; attempt < attempts; attempt++)); do
    local active main_pid running_binary
    active=$(systemctl show "$SERVICE" --property=ActiveState --value 2>/dev/null || true)
    main_pid=$(systemctl show "$SERVICE" --property=MainPID --value 2>/dev/null || true)
    running_binary=
    if [[ "$main_pid" =~ ^[1-9][0-9]*$ ]]; then
      running_binary=$(readlink -f -- "/proc/$main_pid/exe" 2>/dev/null || true)
    fi
    if [ "$active" = active ] && [ "$running_binary" = "$expected_binary" ]; then
      if ready_response_from_pid "$main_pid" "$expected_binary"; then
        return 0
      fi
    fi
    sleep 0.2 || true
  done
  echo "service did not become ready as $expected_binary at $READY_URL" >&2
  return 1
}

verify_service_access () {
  local release=$1
  validate_main_unit || return 1
  local service_user
  if ! service_user=$(systemctl show "$SERVICE" --property=User --value); then
    echo "could not resolve the systemd service identity for $SERVICE" >&2
    return 1
  fi
  if [ -z "$service_user" ] || [ "$service_user" = root ]; then
    echo "$SERVICE must declare an unprivileged service user" >&2
    return 1
  fi
  id -u "$service_user" >/dev/null 2>&1 || {
    echo "systemd service user does not exist: $service_user" >&2
    return 1
  }
  local service_group
  service_group=$(id -gn "$service_user")
  if ! setpriv --reuid="$service_user" --regid="$service_group" --clear-groups \
    --no-new-privs --bounding-set=-all --inh-caps=-all --ambient-caps=-all \
    test -x "$release/bin/deltamud"; then
    echo "service user $service_user cannot traverse or execute $release/bin/deltamud" >&2
    return 1
  fi
}

activate_release () {
  local target=$1
  local activation_mode=${2:-normal}
  local recovery_manifest=${3:-none}
  local content_manifest=${4:-none}
  validate_main_unit || return 1
  validate_runtime_environment || return 1
  validate_release_reference "$target" || return 1
  require_content_approval "$target" || return 1
  if [ "$activation_mode" = post-migration ]; then
    validate_activation_marker_file || return 1
    [ "$(activation_marker_value state)" = migrating ] \
      && [ "$(activation_marker_value target)" = "$target" ] \
      && [ "$(activation_marker_value backup_manifest)" = "$recovery_manifest" ] \
      && [ "$(activation_marker_value content_rollback_manifest)" = "$content_manifest" ] || {
        echo "post-migration activation is not bound to its durable migration marker" >&2
        return 1
      }
  elif [ "$activation_mode" = reconciled ]; then
    validate_activation_marker_file || return 1
    case "$(activation_marker_value state)" in
      blocked|maintenance|migrating|pending) ;;
      *) echo "activation-resolve requires an unresolved activation marker" >&2; return 1 ;;
    esac
    recovery_manifest=$(activation_marker_value backup_manifest) || return 1
    content_manifest=$(activation_marker_value content_rollback_manifest) || return 1
  else
    enter_maintenance_window "$target" || return 1
  fi
  local old=
  if [ -L "$CURRENT_LINK" ]; then
    old=$(readlink -f -- "$CURRENT_LINK") || {
      echo "$CURRENT_LINK is a dangling symbolic link" >&2
      return 1
    }
    validate_release_reference "$old" || return 1
  elif [ -e "$CURRENT_LINK" ]; then
    echo "$CURRENT_LINK exists but is not a symbolic link" >&2
    return 1
  fi
  local content_transition=0
  if [ -z "$old" ] \
    || [ "$(cat -- "$old/CONTENT_SHA256")" != "$(cat -- "$target/CONTENT_SHA256")" ]; then
    content_transition=1
  fi
  local prior_previous=
  local had_previous=0
  if [ -L "$PREVIOUS_LINK" ]; then
    local previous
    previous=$(readlink -f -- "$PREVIOUS_LINK") || {
      echo "$PREVIOUS_LINK is a dangling symbolic link" >&2
      return 1
    }
    validate_release_reference "$previous" || return 1
    prior_previous=$previous
    had_previous=1
  elif [ -e "$PREVIOUS_LINK" ]; then
    echo "$PREVIOUS_LINK exists but is not a symbolic link" >&2
    return 1
  fi

  # Do not repoint MUD_EXEC_PATH while the old process can still copy over.
  # A persistence refusal leaves every link untouched and the old server live.
  stop_service_for_offline_work || return 1

  if [ "$activation_mode" = post-migration ]; then
    validate_backup_manifest "$recovery_manifest" "${target##*/}" any || return 1
    if [ "$content_manifest" != none ]; then
      validate_backup_manifest "$content_manifest" "${target##*/}" any || return 1
    fi
    verify_database_identity || return 1
    [ "$DATABASE_IDENTITY_HASH" = "$(backup_manifest_value "$recovery_manifest" database_identity_sha256)" ] \
      && [ "$DATABASE_ENV_HASH" = "$(backup_manifest_value "$recovery_manifest" database_env_sha256)" ] || {
        echo "post-migration recovery backup no longer matches the runtime database identity" >&2
        return 1
      }
    if [ "$content_manifest" != none ]; then
      [ "$DATABASE_IDENTITY_HASH" = "$(backup_manifest_value "$content_manifest" database_identity_sha256)" ] \
        && [ "$DATABASE_ENV_HASH" = "$(backup_manifest_value "$content_manifest" database_env_sha256)" ] || {
          echo "content rollback backup no longer matches the runtime database identity" >&2
          return 1
        }
    fi
  elif [ "$activation_mode" = reconciled ]; then
    validate_backup_manifest "$recovery_manifest" "${target##*/}" any || return 1
    verify_database_identity || return 1
    [ "$DATABASE_IDENTITY_HASH" = "$(backup_manifest_value "$recovery_manifest" database_identity_sha256)" ] \
      && [ "$DATABASE_ENV_HASH" = "$(backup_manifest_value "$recovery_manifest" database_env_sha256)" ] || {
        echo "reconciled-state checkpoint no longer matches the runtime database identity" >&2
        return 1
      }
    if [ "$content_transition" -eq 1 ] && [ "$content_manifest" = none ]; then
      validate_content_rollback_approval "$target" || return 1
      content_manifest=$CONTENT_APPROVAL_MANIFEST
    fi
  elif [ "$content_transition" -eq 1 ]; then
    validate_content_rollback_approval "$target" || return 1
    content_manifest=$CONTENT_APPROVAL_MANIFEST
    create_release_backup "${target##*/}" stateful || return 1
    validate_backup_manifest "$BACKUP_MANIFEST" "${target##*/}" stateful || return 1
    recovery_manifest=$BACKUP_MANIFEST
  fi

  local next_link="${CURRENT_LINK}.next.$$"
  [ ! -e "$next_link" ] && [ ! -L "$next_link" ] || {
    echo "refusing pre-existing activation link: $next_link" >&2
    return 1
  }
  if ! ln -s -- "$target" "$next_link"; then
    echo "could not prepare the candidate current link" >&2
    return 1
  fi
  ACTIVATION_TEMP_LINKS+=("$next_link")
  local previous_next=
  if [ -n "$old" ] && [ "$old" != "$target" ]; then
    previous_next="${PREVIOUS_LINK}.next.$$"
    [ ! -e "$previous_next" ] && [ ! -L "$previous_next" ] || {
      echo "refusing pre-existing activation link: $previous_next" >&2
      return 1
    }
    if ! ln -s -- "$old" "$previous_next"; then
      echo "could not prepare rollback metadata" >&2
      return 1
    fi
    ACTIVATION_TEMP_LINKS+=("$previous_next")
  fi
  ACTIVATION_TARGET=$target
  ACTIVATION_OLD=$old
  [ -n "$old" ] && ACTIVATION_HAD_CURRENT=1 || ACTIVATION_HAD_CURRENT=0
  ACTIVATION_PRIOR_PREVIOUS=$prior_previous
  ACTIVATION_HAD_PREVIOUS=$had_previous
  # A migration can leave the selected release's content digest unchanged, but
  # the mutable live tree still had a full approval and rollback manifest bound
  # into the migration journal. Revalidate that live tree at the publication
  # boundary for every post-migration activation and any recovery consuming its
  # journaled content manifest, not only content transitions.
  if [ "$activation_mode" = post-migration ] || [ "$content_manifest" != none ] \
      || [ "$content_transition" -eq 1 ]; then
    validate_content_rollback_approval "$target" || return 1
    [ "$content_manifest" = "$CONTENT_APPROVAL_MANIFEST" ] || {
      echo "activation content rollback journal no longer matches the reviewed approval" >&2
      return 1
    }
  fi
  if ! write_activation_marker pending "$target" "$recovery_manifest" "$content_manifest"; then
    echo "candidate was not selected because its activation journal failed" >&2
    return 1
  fi
  ACTIVATION_PENDING=1
  ACTIVATION_RECOVERY_MANIFEST=$recovery_manifest
  ACTIVATION_CONTENT_MANIFEST=$content_manifest
  # Publish rollback metadata first. `current` is the activation commit point.
  # EXIT cleanup restores both links if any later command exits unexpectedly.
  if [ -n "$previous_next" ]; then
    if ! mv -T -- "$previous_next" "$PREVIOUS_LINK"; then
      echo "could not publish rollback metadata" >&2
      return 1
    fi
  fi
  if ! mv -T -- "$next_link" "$CURRENT_LINK" || ! sync -f /opt/deltamud; then
    echo "could not durably select the candidate release" >&2
    return 1
  fi

  if start_service_and_wait "$target/bin/deltamud"; then
    if ! clear_activation_marker "$target"; then
      if [ -e "$ACTIVATION_MARKER" ] \
        && validate_activation_marker_file \
        && [ "$(activation_marker_value state)" = confirmed ] \
        && [ "$(activation_marker_value target)" = "$target" ]; then
        echo "warning: confirmed activation marker remains; run activation-recover" >&2
      else
        echo "candidate is ready but its durable confirmation cannot be read" >&2
        return 1
      fi
    fi
    ACTIVATION_PENDING=0
    ACTIVATION_RECOVERY_MANIFEST=none
    ACTIVATION_CONTENT_MANIFEST=none
    return 0
  fi

  if ! stop_service_and_wait; then
    echo "CRITICAL: failed candidate may still be live; release links were not changed" >&2
    return 1
  fi

  if [ -n "$old" ] && [ -x "$old/bin/deltamud" ]; then
    echo "new release failed activation; restoring $old" >&2
    if [ "$activation_mode" = post-migration ] \
      || [ "$activation_mode" = reconciled ] \
      || [ "$content_transition" -eq 1 ]; then
      restore_activation_selection blocked "$recovery_manifest" "$content_manifest" || return 1
      echo "state-changing activation failed; the old binary was selected and verified stopped" >&2
      echo "restore/reconcile the recorded database and content backups before starting it" >&2
      return 1
    fi
    restore_activation_selection pending none none || return 1
    if ! start_service_and_wait "$old/bin/deltamud"; then
      echo "restored $old but could not restart $SERVICE" >&2
      stop_service_and_wait >/dev/null 2>&1 || true
      write_activation_marker blocked "$old" none none || true
      return 1
    fi
    if ! clear_activation_marker "$old"; then
      if [ -e "$ACTIVATION_MARKER" ] \
        && validate_activation_marker_file \
        && [ "$(activation_marker_value state)" = confirmed ] \
        && [ "$(activation_marker_value target)" = "$old" ]; then
        echo "warning: confirmed rollback marker remains; run activation-recover" >&2
      else
        echo "restored release is live but its durable confirmation cannot be read" >&2
        return 1
      fi
    fi
  else
    echo "new release failed activation and no previous release exists; stopping $SERVICE" >&2
    restore_activation_selection blocked "$recovery_manifest" "$content_manifest" || return 1
  fi
  return 1
}

deploy () {
  [ "$#" -eq 2 ] || usage
  local requested_sha=$1
  local activation=$2
  validate_sha "$requested_sha"
  require_root_paths
  acquire_release_lock
  validate_build_toolchain
  select_build_identity
  local rustc_version
  rustc_version=$(run_as_build_user "$REPO_DIR" rustc --version)
  [[ "$rustc_version" == "rustc 1.98.0 "* ]] || {
    echo "pinned release toolchain is not Rust 1.98.0: $rustc_version" >&2
    exit 77
  }
  # Fetch the requested object into a fresh bare repository. This strips the
  # writable checkout's untracked info/attributes and configuration from the
  # archive boundary. Committed attributes are rejected too, so export-subst or
  # export-ignore can never make an archive differ from its advertised SHA.
  local snapshot_git="$BUILD_WORK/git/snapshot.git"
  run_as_build_user "$BUILD_WORK" git init --bare "$snapshot_git"
  run_as_build_user "$BUILD_WORK" git --git-dir="$snapshot_git" \
    -c protocol.file.allow=always fetch --no-tags "$REPO_DIR" "$requested_sha"
  require_build_user_quiescent || exit 70
  [ "$(run_as_build_user "$BUILD_WORK" git --git-dir="$snapshot_git" \
    rev-parse "$requested_sha^{commit}")" = "$requested_sha" ] || {
      echo "fresh snapshot repository did not resolve the requested commit" >&2
      exit 65
    }
  require_build_user_quiescent || exit 70
  local snapshot_attributes
  if ! snapshot_attributes=$(run_as_build_user "$BUILD_WORK" git --git-dir="$snapshot_git" \
    ls-tree -r --name-only "$requested_sha" -- .gitattributes ':(glob)**/.gitattributes'); then
    echo "could not inspect attributes in the exact release snapshot" >&2
    exit 65
  fi
  require_build_user_quiescent || exit 70
  if [ -n "$snapshot_attributes" ]; then
    echo "release commits may not contain .gitattributes" >&2
    exit 65
  fi

  # Git and tar both remain unprivileged. Only after extraction completes do we
  # reject links and make the exact snapshot root-owned/read-only.
  local source_archive="$BUILD_WORK/tmp/source.tar" source_archive_hash
  run_as_build_user "$BUILD_WORK" git --git-dir="$snapshot_git" archive --format=tar \
    --output="$source_archive" "$requested_sha" -- \
    rust-mud lib deltamud_schema.sql AGREEMENT
  require_build_user_quiescent || exit 70
  source_archive_hash=$(sha256sum -- "$source_archive" | awk '{print $1}') || exit 65
  chown root:root "$source_archive"
  chmod 0444 "$source_archive"
  [ "$source_archive_hash" = "$(sha256sum -- "$source_archive" | awk '{print $1}')" ] || {
    echo "exact-SHA source archive changed while it was frozen" >&2
    exit 65
  }
  run_as_build_user "$SOURCE_ROOT" tar -xf "$source_archive" -C "$SOURCE_ROOT"
  require_build_user_quiescent || {
    echo "snapshot creation left a build-user process behind; release rejected" >&2
    exit 70
  }
  [ "$source_archive_hash" = "$(sha256sum -- "$source_archive" | awk '{print $1}')" ] || {
    echo "frozen source archive changed during extraction" >&2
    exit 65
  }
  rm -f -- "$source_archive"
  local source_links
  if ! source_links=$(find "$SOURCE_ROOT" -type l -print); then
    echo "could not inspect the extracted release snapshot" >&2
    exit 65
  fi
  if [ -n "$source_links" ]; then
    echo "release source snapshot contains symbolic links" >&2
    exit 65
  fi
  chown -R root:root "$SOURCE_ROOT"
  chmod -R u=rwX,go=rX "$SOURCE_ROOT"
  SOURCE_SNAPSHOT_SHA256=$(content_digest "$SOURCE_ROOT") || exit 65
  [ -f "$SOURCE_MUD/Cargo.lock" ] || {
    echo "committed release snapshot has no Cargo.lock" >&2
    exit 65
  }
  if find "$SOURCE_ROOT" -name .cargo -print -quit | grep -q .; then
    echo "release source may not supply Cargo configuration or credential-provider hooks" >&2
    exit 65
  fi
  validate_lockfile_sources "$SOURCE_MUD/Cargo.lock" || {
    echo "release lockfile is malformed or contains a non-crates.io dependency source" >&2
    exit 65
  }
  run_sandboxed_build "$SOURCE_MUD" /bin/true || {
    echo "private-network build sandbox preflight failed" >&2
    exit 77
  }

  # Fetch is the only Cargo phase with host networking, and the exact lockfile
  # is restricted to crates.io with all local Cargo configuration rejected.
  # Every code-executing phase then runs offline in a private-network,
  # minimal-filesystem bubblewrap namespace.
  run_as_build_user "$SOURCE_MUD" cargo fetch --locked
  run_as_build_user "$SOURCE_MUD" "$CARGO_AUDIT_BIN" audit --deny warnings --file Cargo.lock
  require_build_user_quiescent || exit 70
  run_sandboxed_build "$SOURCE_MUD" cargo fmt --all -- --check
  run_sandboxed_build "$SOURCE_MUD" cargo check --all-targets --locked --offline
  run_sandboxed_build "$SOURCE_MUD" cargo test --locked --offline
  run_sandboxed_build "$SOURCE_MUD" cargo test --locked --offline -- --test-threads=1
  run_sandboxed_build "$SOURCE_MUD" /source/rust-mud/scripts/clippy-check.sh
  run_sandboxed_build "$SOURCE_MUD" cargo build --release --locked --offline
  verify_frozen_source_snapshot || exit 65
  require_build_user_quiescent || {
    echo "a build command left a process behind; release rejected" >&2
    exit 70
  }

  local binary="$BUILD_WORK/target/release/deltamud"
  [ -f "$binary" ] && [ ! -L "$binary" ] && [ -x "$binary" ] \
    && [ "$(stat -Lc %u -- "$binary")" -eq "$(id -u "$BUILD_USER")" ] || {
      echo "release build did not produce a regular build-user-owned binary" >&2
      exit 65
    }

  install -d -o root -g root -m 0755 /opt/deltamud "$RELEASE_ROOT"
  validate_root_directory /opt/deltamud
  validate_root_directory "$RELEASE_ROOT"
  local target="$RELEASE_ROOT/$requested_sha"
  [ ! -e "$target" ] && [ ! -L "$target" ] || {
    echo "release already installed: $target" >&2
    exit 66
  }

  DEPLOY_STAGE=$(mktemp -d "$RELEASE_ROOT/.stage.${requested_sha}.XXXXXX")
  chmod 0755 "$DEPLOY_STAGE"
  install -d -o root -g root -m 0755 \
    "$DEPLOY_STAGE/bin" "$DEPLOY_STAGE/docs" "$DEPLOY_STAGE/content"
  install -o root -g root -m 0755 "$binary" "$DEPLOY_STAGE/bin/deltamud"
  local source_binary_hash staged_binary_hash
  source_binary_hash=$(sha256sum -- "$binary" | awk '{print $1}')
  staged_binary_hash=$(sha256sum -- "$DEPLOY_STAGE/bin/deltamud" | awk '{print $1}')
  [ "$source_binary_hash" = "$staged_binary_hash" ] \
    && [ "$source_binary_hash" = "$(sha256sum -- "$binary" | awk '{print $1}')" ] || {
      echo "release binary changed while crossing into the root-owned stage" >&2
      exit 65
    }
  install -o root -g root -m 0644 \
    "$SOURCE_MUD/docs/RUNBOOK.md" "$DEPLOY_STAGE/docs/RUNBOOK.md"
  install -o root -g root -m 0644 \
    "$SOURCE_MUD/COMPATIBILITY.md" "$DEPLOY_STAGE/COMPATIBILITY.md"
  install -o root -g root -m 0644 "$SOURCE_MUD/Cargo.lock" "$DEPLOY_STAGE/Cargo.lock"
  cp -a -- "$SOURCE_ROOT/lib" "$DEPLOY_STAGE/content/lib"
  chown -R root:root "$DEPLOY_STAGE/content/lib"
  chmod -R u=rwX,go=rX "$DEPLOY_STAGE/content/lib"
  content_digest "$DEPLOY_STAGE/content/lib" >"$DEPLOY_STAGE/CONTENT_SHA256"
  chmod 0644 "$DEPLOY_STAGE/CONTENT_SHA256"
  printf '%s\n' "$requested_sha" >"$DEPLOY_STAGE/REVISION"
  chmod 0644 "$DEPLOY_STAGE/REVISION"
  if ! printf '%s\n' \
    'format=deltamud-release-v2' \
    "revision=$requested_sha" \
    "binary_sha256=$staged_binary_hash" \
    "content_sha256=$(cat -- "$DEPLOY_STAGE/CONTENT_SHA256")" \
    >"$DEPLOY_STAGE/RELEASE_MANIFEST"; then
    echo "could not write the release integrity manifest" >&2
    exit 65
  fi
  chmod 0644 "$DEPLOY_STAGE/RELEASE_MANIFEST"
  verify_service_access "$DEPLOY_STAGE"

  local artifacts evidence_digest
  CANARY_ARTIFACT_PARENT=$(mktemp -d "/var/tmp/deltamud-release-canary.${requested_sha}.XXXXXX")
  chown "$BUILD_USER:$BUILD_GROUP" "$CANARY_ARTIFACT_PARENT"
  chmod 0700 "$CANARY_ARTIFACT_PARENT"
  artifacts=$CANARY_ARTIFACT_PARENT/evidence
  install -d -o "$BUILD_USER" -g "$BUILD_GROUP" -m 0700 "$artifacts"
  run_candidate_canary "$DEPLOY_STAGE" "$artifacts"
  verify_frozen_source_snapshot || exit 65
  require_build_user_quiescent || {
    echo "the canary left a build-user process behind; release rejected" >&2
    exit 70
  }
  [ "$staged_binary_hash" = "$(sha256sum -- "$DEPLOY_STAGE/bin/deltamud" | awk '{print $1}')" ] || {
    echo "root-owned release binary changed during canary" >&2
    exit 65
  }
  [ -d "$artifacts" ] && [ ! -L "$artifacts" ] \
    && [ "$(stat -Lc %u -- "$artifacts")" -eq "$BUILD_UID" ] \
    && ! find "$artifacts" \! -type d \! -type f -print -quit | grep -q . \
    && ! find "$artifacts" -type f -links +1 -print -quit | grep -q . || {
      echo "canary evidence contains an unsafe or unexpected filesystem object" >&2
      exit 65
    }
  mv -T -- "$artifacts" "$DEPLOY_STAGE/canary-evidence"
  rmdir -- "$CANARY_ARTIFACT_PARENT"
  CANARY_ARTIFACT_PARENT=
  chown -R root:root "$DEPLOY_STAGE/canary-evidence"
  find "$DEPLOY_STAGE/canary-evidence" -type d -exec chmod 0555 {} +
  find "$DEPLOY_STAGE/canary-evidence" -type f -exec chmod 0444 {} +
  evidence_digest=$(content_digest "$DEPLOY_STAGE/canary-evidence") || exit 65
  if ! printf '%s\n' \
    'format=deltamud-canary-evidence-v1' \
    "revision=$requested_sha" \
    "binary_sha256=$staged_binary_hash" \
    "content_sha256=$(cat -- "$DEPLOY_STAGE/CONTENT_SHA256")" \
    "evidence_sha256=$evidence_digest" >"$DEPLOY_STAGE/CANARY_MANIFEST"; then
    echo "could not bind canary evidence to the release" >&2
    exit 65
  fi
  chmod 0444 "$DEPLOY_STAGE/CANARY_MANIFEST"

  if ! sync -f "$DEPLOY_STAGE"; then
    echo "could not make the completed release stage durable" >&2
    exit 70
  fi
  if ! mv -T -- "$DEPLOY_STAGE" "$target" \
    || ! sync -f "$RELEASE_ROOT"; then
    echo "could not durably publish the installed release" >&2
    exit 70
  fi
  DEPLOY_STAGE=
  validate_installed_release "$target" "$requested_sha"
  artifacts=$target/canary-evidence

  if [ "$activation" = activate ]; then
    critical_activate_release "$target"
    echo "release $requested_sha is active; canary evidence: $artifacts"
  else
    echo "release $requested_sha is installed but not active; canary evidence: $artifacts"
  fi
}

activate_installed () {
  [ "$#" -eq 2 ] || usage
  [ "$2" = --acknowledge-no-state-restore ] || usage
  validate_sha "$1"
  require_root_paths
  acquire_release_lock
  local target="$RELEASE_ROOT/$1"
  validate_installed_release "$target" "$1" || exit 66
  verify_service_access "$target"
  critical_activate_release "$target"
  echo "release $1 is active (no database or runtime-state restore was asserted)"
}

backup_release () {
  [ "$#" -eq 1 ] || usage
  local revision=$1
  validate_sha "$revision"
  require_root_paths
  acquire_release_lock
  local target="$RELEASE_ROOT/$revision"
  validate_installed_release "$target" "$revision" || exit 66
  verify_service_access "$target"
  enter_maintenance_window "$target" || exit 66
  stop_service_for_offline_work || exit 75
  create_release_backup "$revision" || exit 70
  validate_backup_manifest "$BACKUP_MANIFEST" "$revision" || exit 70
  update_maintenance_window "$target" "$BACKUP_MANIFEST" none || exit 70
  echo "service remains stopped for offline reconciliation; backup manifest: $BACKUP_MANIFEST"
}

initialize_backup_release () {
  [ "$#" -eq 2 ] || usage
  local revision=$1
  [ "$2" = --acknowledge-empty-database ] || usage
  validate_sha "$revision"
  require_root_paths
  acquire_release_lock
  local target="$RELEASE_ROOT/$revision"
  validate_installed_release "$target" "$revision" || exit 66
  verify_service_access "$target"
  enter_maintenance_window "$target" || exit 66
  stop_service_for_offline_work || exit 75
  create_release_backup "$revision" empty-initialization || exit 70
  validate_backup_manifest "$BACKUP_MANIFEST" "$revision" empty-initialization || exit 70
  update_maintenance_window "$target" "$BACKUP_MANIFEST" none || exit 70
  echo "service remains stopped for initial content reconciliation; empty-database backup manifest: $BACKUP_MANIFEST"
}

approve_content () {
  [ "$#" -eq 3 ] || usage
  local revision=$1
  local manifest=$2
  local acknowledgement=$3
  validate_sha "$revision"
  [ "$acknowledgement" = --acknowledge-reviewed-runtime-merge ] || usage
  require_root_paths
  acquire_release_lock
  local target="$RELEASE_ROOT/$revision"
  validate_installed_release "$target" "$revision" || exit 66
  validate_activation_marker_file || exit 66
  case "$(activation_marker_value state)" in
    maintenance|blocked) ;;
    *) echo "content approval requires a maintenance or blocked recovery journal" >&2; exit 66 ;;
  esac
  [ "$(activation_marker_value target)" = "$target" ] \
    && [ "$(activation_marker_value backup_manifest)" = "$manifest" ] || {
      echo "content approval is not bound to the target's maintenance backup" >&2
      exit 66
    }
  verify_service_stopped || {
    echo "content approval requires $SERVICE to remain fully stopped" >&2
    exit 75
  }
  validate_backup_manifest "$manifest" "$revision" any || exit 66
  verify_database_identity || exit 66
  [ "$DATABASE_IDENTITY_HASH" = "$(backup_manifest_value "$manifest" database_identity_sha256)" ] \
    && [ "$DATABASE_ENV_HASH" = "$(backup_manifest_value "$manifest" database_env_sha256)" ] || {
      echo "database identity changed since the content backup" >&2
      exit 66
    }
  validate_root_directory /etc || exit 77
  validate_root_directory /etc/deltamud || exit 77
  local digest live_digest database_hash lib_hash generation boot_id invocation started approved_epoch
  digest=$(cat -- "$target/CONTENT_SHA256")
  live_digest=$(content_digest /var/lib/deltamud/lib)
  database_hash=$(backup_manifest_value "$manifest" database_sha256)
  lib_hash=$(backup_manifest_value "$manifest" lib_sha256)
  generation=$(service_generation) || exit 77
  IFS='|' read -r boot_id invocation started <<<"$generation"
  approved_epoch=$(date +%s) || exit 70
  APPROVAL_TEMP=$(mktemp /etc/deltamud/.content-approved.XXXXXX)
  printf '%s\n' \
    'format=deltamud-content-approval-v1' \
    "content_sha256=$digest" \
    "revision=$revision" \
    "backup_manifest=$manifest" \
    "database_sha256=$database_hash" \
    "lib_sha256=$lib_hash" \
    "live_digest=$live_digest" \
    "approved_epoch=$approved_epoch" \
    "boot_id=$boot_id" \
    "service_invocation_id=$invocation" \
    "service_start_monotonic=$started" >"$APPROVAL_TEMP"
  chmod 0644 "$APPROVAL_TEMP"
  sync -f "$APPROVAL_TEMP"
  verify_service_stopped || {
    echo "service state changed while content approval was being recorded" >&2
    exit 75
  }
  [ "$generation" = "$(service_generation)" ] || {
    echo "service generation changed while content approval was being recorded" >&2
    exit 75
  }
  mv -T -- "$APPROVAL_TEMP" "$CONTENT_APPROVAL"
  APPROVAL_TEMP=
  sync -f /etc/deltamud
  write_activation_marker "$(activation_marker_value state)" "$target" "$manifest" \
    "$manifest" || exit 70
  echo "approved release content digest $digest after reviewed offline reconciliation"
  echo "$SERVICE remains stopped"
  if [ "$(backup_manifest_value "$manifest" backup_kind)" = empty-initialization ]; then
    echo "for initial schema creation, run initialize-migrate-activate $revision --acknowledge-empty-database"
  else
    echo "if this revision requires schema work, run migrate-activate $revision --acknowledge-reconciled-state"
  fi
  echo "otherwise finish through activation-resolve $revision --acknowledge-reconciled-state"
}

migrate_activate_common () {
  local revision=$1
  local backup_kind=$2
  local allow_existing_maintenance=${3:-0}
  validate_sha "$revision"
  require_root_paths
  acquire_release_lock
  local target="$RELEASE_ROOT/$revision"
  validate_installed_release "$target" "$revision" || exit 66
  verify_service_access "$target"
  validate_migration_unit "$revision" || exit 77

  local resume_manifest=none resume_content_manifest=none marker_state=
  if [ -e "$ACTIVATION_MARKER" ] || [ -L "$ACTIVATION_MARKER" ]; then
    [ "$allow_existing_maintenance" -eq 1 ] || {
      ensure_no_activation_marker
      exit 66
    }
    validate_activation_marker_file || exit 66
    marker_state=$(activation_marker_value state) || exit 66
    [ "$(activation_marker_value target)" = "$target" ] || {
      echo "migration resume requires this target's maintenance journal" >&2
      exit 66
    }
    resume_manifest=$(activation_marker_value backup_manifest) || exit 66
    [ "$resume_manifest" != none ] || {
      echo "migration resume requires a completed maintenance backup" >&2
      exit 66
    }
    validate_backup_manifest "$resume_manifest" "$revision" "$backup_kind" historical || exit 66
    resume_content_manifest=$(activation_marker_value content_rollback_manifest) || exit 66
    if [ "$resume_content_manifest" != none ]; then
      validate_backup_manifest "$resume_content_manifest" "$revision" any historical || exit 66
    fi
    verify_service_stopped || {
      echo "migration resume requires $SERVICE to remain fully stopped" >&2
      exit 75
    }
    case "$marker_state" in
      maintenance)
        ;;
      migrating|blocked)
        local marker_attempt migration_unit retry_snapshot
        if [ "$marker_state" = migrating ]; then
          marker_attempt=$(activation_marker_value attempt) || exit 66
          case "$marker_attempt" in
            ready|consumed) ;;
            *) echo "migration retry found an invalid one-shot attempt state" >&2; exit 66 ;;
          esac
        fi
        migration_unit="deltamud-migrate@${revision}.service"
        retry_snapshot=$(stable_terminal_migration_snapshot "$migration_unit") || {
          echo "$migration_unit is running, queued, changing, or unreadable; migration retry refused" >&2
          exit 75
        }
        # Revoke the old one-shot authorization before any reset, baseline,
        # backup, or other fallible retry preparation. Direct service and
        # migration starts now both fail closed while the manager holds its
        # exclusive lock.
        write_activation_marker maintenance "$target" "$resume_manifest" \
          "$resume_content_manifest" || exit 70
        echo "retrying terminal migration state after explicit reconciliation: $retry_snapshot" >&2
        ;;
      *)
        echo "migration resume accepts only this target's maintenance, blocked, or terminal migrating journal" >&2
        exit 66
        ;;
    esac
  else
    enter_maintenance_window "$target" || exit 66
  fi
  begin_critical_phase
  stop_service_for_offline_work || exit 75
  create_release_backup "$revision" "$backup_kind" || exit 70
  validate_backup_manifest "$BACKUP_MANIFEST" "$revision" "$backup_kind" || exit 70
  # Make the fresh recovery point durable in the still-non-startable journal
  # before any later identity, approval, reset, or unit operation can fail.
  update_maintenance_window "$target" "$BACKUP_MANIFEST" none || exit 70
  verify_database_identity || exit 70
  [ "$DATABASE_IDENTITY_HASH" = "$(backup_manifest_value "$BACKUP_MANIFEST" database_identity_sha256)" ] \
    && [ "$DATABASE_ENV_HASH" = "$(backup_manifest_value "$BACKUP_MANIFEST" database_env_sha256)" ] || {
      echo "database identity changed after backup; migration canceled" >&2
      exit 70
    }

  # Every schema-changing run is bound to an exact-revision content approval,
  # stopped service generation, live-tree digest, and verified rollback
  # manifest, even when the selected release happens to share that digest.
  validate_content_rollback_approval "$target" || exit 66
  local migration_content_manifest=$CONTENT_APPROVAL_MANIFEST

  local migration_unit="deltamud-migrate@${revision}.service"
  update_maintenance_window "$target" "$BACKUP_MANIFEST" \
    "$migration_content_manifest" || exit 70
  if ! systemctl reset-failed "$migration_unit" >/dev/null 2>&1; then
    local deferred_rc=0
    end_critical_phase || deferred_rc=$?
    echo "could not reset the migration unit before capturing a new baseline" >&2
    [ "$deferred_rc" -eq 0 ] || return "$deferred_rc"
    return 70
  fi
  local before_invocation before_started
  before_invocation=$(systemctl show "$migration_unit" --property=InvocationID --value) || {
    local deferred_rc=0
    end_critical_phase || deferred_rc=$?
    echo "could not capture the migration unit's baseline invocation" >&2
    [ "$deferred_rc" -eq 0 ] || return "$deferred_rc"
    return 70
  }
  before_started=$(systemctl show "$migration_unit" --property=ExecMainStartTimestampMonotonic --value) || {
    local deferred_rc=0
    end_critical_phase || deferred_rc=$?
    echo "could not capture the migration unit's baseline start time" >&2
    [ "$deferred_rc" -eq 0 ] || return "$deferred_rc"
    return 70
  }
  # Publish a single start authorization only after the failed state has been
  # reset and the old invocation identity is known.
  write_activation_marker migrating "$target" "$BACKUP_MANIFEST" \
    "$migration_content_manifest" || {
      local deferred_rc=0
      end_critical_phase || deferred_rc=$?
      echo "could not authorize the new migration invocation" >&2
      [ "$deferred_rc" -eq 0 ] || return "$deferred_rc"
      return 70
    }
  local queue_rc=0
  queue_systemd_job start "$migration_unit" || queue_rc=$?
  if [ "$queue_rc" -ne 0 ]; then
    echo "migration queue client returned status $queue_rc; observing the unit before deciding its outcome" >&2
  fi
  local migration_ok=0 migration_invocation_observed=0
  local migration_result migration_status migration_code migration_invocation migration_started migration_active
  local migration_job migration_pid first_terminal second_terminal
  local attempt=0
  # A condition skip with a stable terminal unit is bounded. A queued job or a
  # consumed authorization remains under observation: abandoning either could
  # let DDL begin after the manager released its lock. Once this invocation is
  # observed, wait without a deadline so in-flight DDL is never killed merely
  # for taking a long time.
  while :; do
    attempt=$((attempt + 1))
    first_terminal=$(migration_unit_snapshot "$migration_unit") || {
      sleep 0.2 || true
      continue
    }
    IFS='|' read -r migration_active migration_pid migration_job migration_invocation \
      migration_result migration_code migration_status migration_started <<<"$first_terminal"
    if [ -n "$migration_invocation" ] \
      && [ "$migration_invocation" != "$before_invocation" ] \
      && { { [[ "$migration_started" =~ ^[1-9][0-9]*$ ]] \
             && [ "$migration_started" != "$before_started" ]; } \
           || migration_marker_matches consumed "$target" "$BACKUP_MANIFEST" \
             "$migration_content_manifest"; }; then
      migration_invocation_observed=1
      break
    fi
    if [ $((attempt % 50)) -eq 0 ]; then
      echo "still waiting for $migration_unit to begin its newly authorized ExecStart (active=${migration_active:-unknown}, job=${migration_job:-unknown})" >&2
    fi
    if [ "$attempt" -ge 300 ]; then
      if migration_marker_matches consumed "$target" "$BACKUP_MANIFEST" \
        "$migration_content_manifest"; then
        echo "$migration_unit consumed its authorization; continuing until its invocation is observable" >&2
      elif migration_marker_matches ready "$target" "$BACKUP_MANIFEST" \
        "$migration_content_manifest"; then
        local no_start_snapshot
        if no_start_snapshot=$(stable_terminal_migration_snapshot "$migration_unit") \
          && migration_marker_matches ready "$target" "$BACKUP_MANIFEST" \
            "$migration_content_manifest" \
          && write_activation_marker maintenance "$target" "$BACKUP_MANIFEST" \
            "$migration_content_manifest" \
          && validate_activation_marker_file \
          && [ "$(activation_marker_value state)" = maintenance ] \
          && [ "$(activation_marker_value target)" = "$target" ] \
          && [ "$(activation_marker_value backup_manifest)" = "$BACKUP_MANIFEST" ] \
          && [ "$(activation_marker_value content_rollback_manifest)" = "$migration_content_manifest" ]; then
          echo "$migration_unit reached a stable no-start terminal state; authorization revoked" >&2
          break
        fi
        echo "$migration_unit remains queued, is changing, or could not be safely revoked; continuing to observe it" >&2
      else
        echo "migration authorization changed unexpectedly while awaiting ExecStart" >&2
        break
      fi
    fi
    sleep 0.2 || true
  done
  if [ "$migration_invocation_observed" -ne 1 ]; then
    local deferred_rc=0
    end_critical_phase || deferred_rc=$?
    echo "$migration_unit did not begin a new ExecStart within 60 seconds; service remains stopped" >&2
    echo "active=${migration_active:-unknown} job=${migration_job:-unknown} result=${migration_result:-unknown}" >&2
    [ "$deferred_rc" -eq 0 ] || return "$deferred_rc"
    return 70
  fi
  attempt=0
  while :; do
    attempt=$((attempt + 1))
    first_terminal=$(migration_unit_snapshot "$migration_unit") || {
      if [ $((attempt % 150)) -eq 0 ]; then
        echo "still waiting for a reliable $migration_unit state snapshot" >&2
      fi
      sleep 0.2 || true
      continue
    }
    IFS='|' read -r migration_active migration_pid migration_job migration_invocation \
      migration_result migration_code migration_status migration_started <<<"$first_terminal"
    if [ -n "$migration_invocation" ] \
      && [ "$migration_invocation" != "$before_invocation" ] \
      && { [ "$migration_active" = inactive ] || [ "$migration_active" = failed ]; } \
      && [ "$migration_pid" = 0 ] \
      && [ -z "$migration_job" ]; then
      sleep 0.2 || true
      second_terminal=$(migration_unit_snapshot "$migration_unit") || continue
      [ "$first_terminal" = "$second_terminal" ] || continue
      # systemd exposes CLD_EXITED as the numeric waitid code 1.
      if [ "$migration_active" = inactive ] \
        && [ "$migration_result" = success ] \
        && [ "$migration_code" = 1 ] \
        && [ "$migration_status" = 0 ] \
        && [[ "$migration_started" =~ ^[1-9][0-9]*$ ]] \
        && [ "$migration_started" != "$before_started" ] \
        && migration_marker_matches consumed "$target" "$BACKUP_MANIFEST" \
          "$migration_content_manifest"; then
        migration_ok=1
      fi
      break
    fi
    if [ $((attempt % 150)) -eq 0 ]; then
      echo "still waiting for $migration_unit to reach a stable terminal state (active=${migration_active:-unknown}, pid=${migration_pid:-unknown})" >&2
    fi
    sleep 0.2 || true
  done
  if [ "$migration_ok" -ne 1 ]; then
    local deferred_rc=0
    end_critical_phase || deferred_rc=$?
    echo "schema migration did not report a clean result; service remains stopped" >&2
    echo "result=${migration_result:-unknown} code=${migration_code:-unknown} status=${migration_status:-unknown}" >&2
    echo "inspect the migration unit and restore $BACKUP_MANIFEST if its durable outcome is unsafe" >&2
    [ "$deferred_rc" -eq 0 ] || return "$deferred_rc"
    return 70
  fi

  local activation_rc=0
  if ! activate_release "$target" post-migration "$BACKUP_MANIFEST" \
    "$migration_content_manifest"; then
    activation_rc=1
  fi
  local deferred_rc=0
  end_critical_phase || deferred_rc=$?
  [ "$deferred_rc" -eq 0 ] || return "$deferred_rc"
  [ "$activation_rc" -eq 0 ] || {
    echo "post-migration activation failed; recovery backup: $BACKUP_MANIFEST" >&2
    return "$activation_rc"
  }
  echo "schema migration and release activation completed for $revision"
  echo "verified pre-migration backup: $BACKUP_MANIFEST"
}

migrate_activate () {
  [ "$#" -ge 1 ] && [ "$#" -le 2 ] || usage
  local allow_existing=0
  if [ "$#" -eq 2 ]; then
    [ "$2" = --acknowledge-reconciled-state ] || usage
    allow_existing=1
  fi
  migrate_activate_common "$1" stateful "$allow_existing"
}

initialize_migrate_activate () {
  [ "$#" -eq 2 ] || usage
  [ "$2" = --acknowledge-empty-database ] || usage
  migrate_activate_common "$1" empty-initialization 1
}

rollback () {
  [ "$#" -eq 2 ] || usage
  [ "$2" = --acknowledge-no-state-restore ] || usage
  validate_sha "$1"
  require_root_paths
  acquire_release_lock
  local target="$RELEASE_ROOT/$1"
  validate_installed_release "$target" "$1" || exit 66
  echo "schema-compatible rollback acknowledged: no database or runtime-state restore may precede this command" >&2
  critical_activate_release "$target"
  echo "rolled back to $1"
}

bootstrap_implementor () {
  [ "$#" -eq 3 ] || usage
  local revision=$1
  local player_name=$2
  [ "$3" = --acknowledge-offline-authority-bootstrap ] || usage
  validate_sha "$revision"
  [[ "$player_name" =~ ^[A-Za-z]{2,20}$ ]] || {
    echo "bootstrap player name must contain 2-20 ASCII letters" >&2
    exit 64
  }
  require_root_paths
  acquire_release_lock
  local target="$RELEASE_ROOT/$revision"
  validate_installed_release "$target" "$revision" || exit 66
  verify_service_access "$target"
  [ -L "$CURRENT_LINK" ] \
    && [ "$(readlink -f -- "$CURRENT_LINK")" = "$target" ] || {
      echo "authority bootstrap must use the exact currently selected installed release" >&2
      exit 66
    }
  enter_maintenance_window "$target" || exit 66
  begin_critical_phase
  stop_service_for_offline_work || exit 75
  create_release_backup "$revision" stateful || exit 70
  validate_backup_manifest "$BACKUP_MANIFEST" "$revision" stateful || exit 70
  update_maintenance_window "$target" "$BACKUP_MANIFEST" none || exit 70

  local bootstrap_rc=0
  if ! run_installed_maintenance "$target" --bootstrap-implementor "$player_name"; then
    bootstrap_rc=1
  fi
  local deferred_rc=0
  end_critical_phase || deferred_rc=$?
  [ "$deferred_rc" -eq 0 ] || return "$deferred_rc"
  [ "$bootstrap_rc" -eq 0 ] || {
    echo "Implementor bootstrap failed; $SERVICE remains stopped" >&2
    echo "recovery backup: $BACKUP_MANIFEST" >&2
    return 70
  }
  clear_maintenance_window "$target" || return 70
  echo "bootstrapped $player_name through installed release $revision"
  echo "$SERVICE remains stopped; verified backup: $BACKUP_MANIFEST"
}

recover_confirmed_activation () {
  [ "$#" -eq 0 ] || usage
  require_root_paths
  acquire_release_lock
  validate_main_unit || exit 77
  validate_runtime_environment || exit 77
  validate_activation_marker_file || exit 66
  local state target binary_hash
  state=$(activation_marker_value state) || exit 66
  target=$(activation_marker_value target) || exit 66
  binary_hash=$(activation_marker_value binary_sha256) || exit 66
  [ "$state" = confirmed ] || {
    echo "activation-recover only clears an already confirmed marker" >&2
    exit 66
  }
  validate_marker_target "$target" "$binary_hash" || exit 66
  wait_ready "$target/bin/deltamud" || {
    echo "confirmed target is not currently the exact ready MainPID; marker retained" >&2
    exit 75
  }
  clear_activation_marker "$target" || exit 70
  echo "cleared the stale confirmed activation marker for ${target##*/}"
}

resolve_activation () {
  [ "$#" -eq 2 ] || usage
  local revision=$1
  local acknowledgement=$2
  validate_sha "$revision"
  [ "$acknowledgement" = --acknowledge-reconciled-state ] || usage
  require_root_paths
  acquire_release_lock
  local target="$RELEASE_ROOT/$revision"
  validate_installed_release "$target" "$revision" || exit 66
  verify_service_access "$target" || exit 77
  verify_service_stopped || {
    echo "activation-resolve requires $SERVICE to be fully stopped" >&2
    exit 75
  }
  validate_activation_marker_file || exit 66
  local marker_state recovery_manifest
  marker_state=$(activation_marker_value state) || exit 66
  case "$marker_state" in
    blocked|maintenance|migrating|pending) ;;
    *) echo "there is no unresolved activation to reconcile" >&2; exit 66 ;;
  esac
  local journal_target
  journal_target=$(activation_marker_value target) || exit 66
  if [ "$marker_state" = migrating ]; then
    local marker_target marker_revision migration_unit terminal_state marker_attempt
    local active main_pid job invocation result code status started
    marker_target=$(activation_marker_value target) || exit 66
    marker_revision=${marker_target##*/}
    validate_sha "$marker_revision"
    migration_unit="deltamud-migrate@${marker_revision}.service"
    terminal_state=$(stable_terminal_migration_snapshot "$migration_unit") || {
        echo "$migration_unit is still running, queued, or changing; activation-resolve refused" >&2
        echo "wait for a terminal state and inspect the recorded backup before retrying" >&2
        exit 75
      }
    IFS='|' read -r active main_pid job invocation result code status started <<<"$terminal_state"
    if [ "$marker_target" = "$target" ]; then
      marker_attempt=$(activation_marker_value attempt) || exit 66
      [ "$marker_attempt" = consumed ] \
        && [ "$active" = inactive ] \
        && [ "$result" = success ] \
        && [ "$code" = 1 ] \
        && [ "$status" = 0 ] \
        && [[ "$started" =~ ^[1-9][0-9]*$ ]] || {
          echo "$migration_unit did not prove a consumed, clean migration; activation of its target is forbidden" >&2
          echo "restore/reconcile the recorded backup, then rerun migrate-activate with its explicit reconciliation acknowledgement" >&2
          exit 75
        }
    else
      echo "activation-resolve is selecting an explicitly reconciled alternate target after a terminal migration incident" >&2
    fi
  fi
  local recovery_kind content_manifest
  content_manifest=$(activation_marker_value content_rollback_manifest) || exit 66
  if [ "$journal_target" != "$target" ]; then
    echo "activation-resolve is checkpointing an explicitly reconciled alternate target" >&2
    content_manifest=none
  fi
  recovery_kind=$(current_database_backup_kind) || {
    echo "could not classify the reconciled database for its pre-retry checkpoint" >&2
    exit 70
  }
  create_release_backup "$revision" "$recovery_kind" || exit 70
  validate_backup_manifest "$BACKUP_MANIFEST" "$revision" "$recovery_kind" || exit 70
  recovery_manifest=$BACKUP_MANIFEST
  write_activation_marker blocked "$target" "$recovery_manifest" "$content_manifest" || exit 70

  # Retarget the durable journal before asking for a replacement approval. This
  # makes recovery from a rejected content transition possible even when the
  # single current approval belongs to the failed candidate. A second resolve
  # consumes the new approval and performs the activation.
  local approval_ready=1 current_release=
  require_content_approval "$target" || approval_ready=0
  if [ "$approval_ready" -eq 1 ] && [ -L "$CURRENT_LINK" ]; then
    current_release=$(readlink -f -- "$CURRENT_LINK") || exit 66
    validate_release_reference "$current_release" || exit 66
    if [ "$(cat -- "$current_release/CONTENT_SHA256")" \
      != "$(cat -- "$target/CONTENT_SHA256")" ]; then
      validate_content_rollback_approval "$target" || approval_ready=0
    fi
  elif [ -e "$CURRENT_LINK" ]; then
    echo "$CURRENT_LINK exists but is not a symbolic link" >&2
    exit 66
  elif [ "$approval_ready" -eq 1 ]; then
    validate_content_rollback_approval "$target" || approval_ready=0
  fi
  if [ "$approval_ready" -ne 1 ]; then
    echo "recovery journal retargeted to $revision with fresh checkpoint $recovery_manifest" >&2
    echo "review/reconcile the stopped runtime tree, then run:" >&2
    echo "  $TRUSTED_MANAGER content-approve $revision $recovery_manifest --acknowledge-reviewed-runtime-merge" >&2
    echo "then repeat activation-resolve for $revision; $SERVICE remains stopped" >&2
    return 75
  fi
  critical_activate_release "$target" reconciled "$recovery_manifest"
  echo "reconciled state and activated $revision"
}

main () {
  [ "$#" -ge 1 ] || usage
  require_trusted_manager
  case "$1" in
    activation-guard)
      [ "$#" -eq 1 ] || exit 64
      require_root_paths
      validate_runtime_environment || exit 77
      activation_guard
      return
      ;;
    activation-confirm)
      [ "$#" -eq 1 ] || exit 64
      require_root_paths
      validate_runtime_environment || exit 77
      activation_confirm
      return
      ;;
    migration-guard)
      [ "$#" -eq 2 ] || exit 64
      require_root_paths
      migration_guard "$2"
      return
      ;;
  esac
  trap cleanup_release EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  local command=$1
  shift
  case "$command" in
    deploy)
      [ "$#" -eq 2 ] && [ "$2" = --acknowledge-no-state-restore ] || usage
      deploy "$1" activate
      ;;
    install) [ "$#" -eq 1 ] || usage; deploy "$1" install-only ;;
    backup) backup_release "$@" ;;
    initialize-backup) initialize_backup_release "$@" ;;
    content-approve) approve_content "$@" ;;
    activate) activate_installed "$@" ;;
    migrate-activate) migrate_activate "$@" ;;
    initialize-migrate-activate) initialize_migrate_activate "$@" ;;
    rollback) rollback "$@" ;;
    bootstrap-implementor) bootstrap_implementor "$@" ;;
    activation-recover) recover_confirmed_activation "$@" ;;
    activation-resolve) resolve_activation "$@" ;;
    *) usage ;;
  esac
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
