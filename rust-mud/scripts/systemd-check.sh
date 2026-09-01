#!/usr/bin/env bash
# Validate the checked-in production units in an isolated systemd root and
# prove that the trusted release manager pins their exact contents.
set -Eeuo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
MUD_DIR=$(cd "$SCRIPT_DIR/.." && pwd)
MAIN_UNIT="$MUD_DIR/deploy/systemd/deltamud.service"
MIGRATION_UNIT="$MUD_DIR/deploy/systemd/deltamud-migrate@.service"
RELEASE_MANAGER="$SCRIPT_DIR/release.sh"

for command in systemd-analyze sha256sum; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "required command is unavailable: $command" >&2
    exit 1
  fi
done
[ -x /usr/bin/python3 ] || {
  echo "required interpreter is unavailable: /usr/bin/python3" >&2
  exit 1
}

SYSTEM_UNIT_DIR=
for candidate in /usr/lib/systemd/system /lib/systemd/system; do
  if [[ -d "$candidate" ]]; then
    SYSTEM_UNIT_DIR=$candidate
    break
  fi
done
if [[ -z "$SYSTEM_UNIT_DIR" ]]; then
  echo "systemd's packaged unit directory is unavailable" >&2
  exit 1
fi

CHECK_ROOT=$(mktemp -d /var/tmp/deltamud-systemd-check.XXXXXX)
cleanup() {
  case "$CHECK_ROOT" in
    /var/tmp/deltamud-systemd-check.*)
      rm -rf -- "$CHECK_ROOT"
      ;;
    *)
      echo "refusing to clean unexpected systemd-check path: $CHECK_ROOT" >&2
      return 1
      ;;
  esac
}
trap cleanup EXIT HUP INT TERM

install -d -m 0755 \
  "$CHECK_ROOT/etc/systemd/system" \
  "$CHECK_ROOT/etc/deltamud" \
  "$CHECK_ROOT/opt/deltamud/current/bin" \
  "$CHECK_ROOT/opt/deltamud/releases/test_instance/bin" \
  "$CHECK_ROOT/usr/local/sbin" \
  "$CHECK_ROOT/usr/lib/systemd" \
  "$CHECK_ROOT/var/lib/deltamud"
cp -a "$SYSTEM_UNIT_DIR" "$CHECK_ROOT/usr/lib/systemd/system"
install -m 0644 "$MAIN_UNIT" "$MIGRATION_UNIT" \
  "$CHECK_ROOT/etc/systemd/system/"
install -m 0755 /bin/true "$CHECK_ROOT/opt/deltamud/current/bin/deltamud"
install -m 0755 /bin/true \
  "$CHECK_ROOT/opt/deltamud/releases/test_instance/bin/deltamud"
install -m 0755 /bin/true "$CHECK_ROOT/usr/local/sbin/deltamud-release"
printf '%s\n' 'DATABASE_URL=mysql://example.invalid/deltamud' \
  >"$CHECK_ROOT/etc/deltamud/deltamud.env"

systemd-analyze verify --root="$CHECK_ROOT" \
  "$CHECK_ROOT/etc/systemd/system/deltamud.service" \
  "$CHECK_ROOT/etc/systemd/system/deltamud-migrate@.service"

main_hash=$(sha256sum "$MAIN_UNIT" | awk '{print $1}')
migration_hash=$(sha256sum "$MIGRATION_UNIT" | awk '{print $1}')
grep -Fxq "MAIN_UNIT_SHA256=$main_hash" "$RELEASE_MANAGER" || {
  echo "release manager's main-unit hash is stale" >&2
  exit 1
}
grep -Fxq "MIGRATION_UNIT_SHA256=$migration_hash" "$RELEASE_MANAGER" || {
  echo "release manager's migration-unit hash is stale" >&2
  exit 1
}

write_backup_fixture() {
  local name=$1
  shift
  FIXTURE_PATH="$CHECK_ROOT/$name.cnf"
  printf '%s\n' "$@" >"$FIXTURE_PATH"
  chmod 0600 "$FIXTURE_PATH"
}

write_client_fixture() {
  local name=$1
  local protocol=$2
  local host=$3
  local port=$4
  shift 4
  write_backup_fixture "$name" \
    '[client]' \
    "protocol=$protocol" \
    "host=$host" \
    "port=$port" \
    'user=test_backup' \
    'password=TEST_ONLY_DO_NOT_PRINT' \
    "$@"
}

parse_backup_fixture() (
  local fixture=$1
  # release.sh is source-safe: its main dispatch is guarded by BASH_SOURCE.
  # Keep its fixed production globals and PATH changes inside this subshell.
  source "$RELEASE_MANAGER"
  parse_backup_config_endpoint "$fixture" || exit 1
  printf '%s\t%s\n' "$BACKUP_ENDPOINT_HOST" "$BACKUP_ENDPOINT_PORT"
)

backup_fixture_matches_runtime() (
  local fixture=$1
  local runtime_host=$2
  local runtime_port=$3
  local runtime_output
  local runtime_endpoint=()
  source "$RELEASE_MANAGER"
  parse_backup_config_endpoint "$fixture" || exit 1
  runtime_output=$(normalize_database_endpoint "$runtime_host" "$runtime_port") || exit 1
  mapfile -t runtime_endpoint <<<"$runtime_output"
  [ "${#runtime_endpoint[@]}" -eq 2 ] || exit 1
  database_endpoints_match "$BACKUP_ENDPOINT_HOST" "$BACKUP_ENDPOINT_PORT" \
    "${runtime_endpoint[0]}" "${runtime_endpoint[1]}"
)

expect_fixture_endpoint() {
  local label=$1
  local fixture=$2
  local expected=$3
  local actual
  if ! actual=$(parse_backup_fixture "$fixture"); then
    echo "backup config fixture was unexpectedly rejected: $label" >&2
    exit 1
  fi
  [ "$actual" = "$expected" ] || {
    echo "backup config fixture normalized incorrectly: $label" >&2
    exit 1
  }
  [[ "$actual" != *TEST_ONLY_DO_NOT_PRINT* ]] || {
    echo "backup config parser exposed its fixture password: $label" >&2
    exit 1
  }
}

expect_fixture_rejected() {
  local label=$1
  local fixture=$2
  if parse_backup_fixture "$fixture" >/dev/null 2>&1; then
    echo "unsafe backup config fixture was accepted: $label" >&2
    exit 1
  fi
}

write_client_fixture canonical tcp 127.0.0.1 3306
canonical_fixture=$FIXTURE_PATH
expect_fixture_endpoint canonical "$canonical_fixture" $'127.0.0.1\t3306'
backup_fixture_matches_runtime "$canonical_fixture" 127.0.0.1 3306 || {
  echo "canonical backup/runtime database endpoints did not correlate" >&2
  exit 1
}

write_client_fixture normalized tcp DB.Example.COM. 03306
expect_fixture_endpoint normalized "$FIXTURE_PATH" $'db.example.com\t3306'

write_client_fixture wrong-protocol socket 127.0.0.1 3306
expect_fixture_rejected wrong-protocol "$FIXTURE_PATH"

write_client_fixture invalid-host tcp 'bad/host' 3306
expect_fixture_rejected invalid-host "$FIXTURE_PATH"

write_client_fixture invalid-port tcp 127.0.0.1 65536
expect_fixture_rejected invalid-port "$FIXTURE_PATH"

write_client_fixture duplicate-host tcp 127.0.0.1 3306 'host=127.0.0.2'
expect_fixture_rejected duplicate-host "$FIXTURE_PATH"

write_backup_fixture malformed \
  '[client]' 'protocol=tcp' 'host=127.0.0.1' 'port=3306' \
  'user=test_backup' 'password-without-equals'
expect_fixture_rejected malformed "$FIXTURE_PATH"

write_client_fixture wrong-host tcp 127.0.0.2 3306
if backup_fixture_matches_runtime "$FIXTURE_PATH" 127.0.0.1 3306; then
  echo "backup/runtime database endpoint correlation accepted the wrong host" >&2
  exit 1
fi

write_client_fixture wrong-port tcp 127.0.0.1 3307
if backup_fixture_matches_runtime "$FIXTURE_PATH" 127.0.0.1 3306; then
  echo "backup/runtime database endpoint correlation accepted the wrong port" >&2
  exit 1
fi

sed -n '/^validate_backup_config () {/,/^}/p' "$RELEASE_MANAGER" \
  | grep -F 'parse_backup_config_endpoint "$BACKUP_CNF"' >/dev/null || {
    echo "release manager no longer binds backup validation to the strict parser" >&2
    exit 1
  }
sed -n '/^verify_database_identity () {/,/^}/p' "$RELEASE_MANAGER" \
  | grep -F 'database_endpoints_match "$BACKUP_ENDPOINT_HOST" "$BACKUP_ENDPOINT_PORT"' \
    >/dev/null || {
    echo "release manager no longer correlates normalized backup/runtime endpoints" >&2
    exit 1
  }

write_lock_fixture() {
  local name=$1
  shift
  LOCK_FIXTURE="$CHECK_ROOT/$name.lock"
  printf '%s\n' "$@" >"$LOCK_FIXTURE"
}

lock_fixture_is_accepted() (
  source "$RELEASE_MANAGER"
  validate_lockfile_sources "$1"
)

write_lock_fixture crates-only \
  'version = 4' \
  '[[package]]' \
  'name = "deltamud"' \
  'version = "0.1.0"' \
  '[[package]]' \
  'name = "anyhow"' \
  'version = "1.0.0"' \
  'source = "registry+https://github.com/rust-lang/crates.io-index"'
lock_fixture_is_accepted "$LOCK_FIXTURE" || {
  echo "crates.io-only lockfile fixture was unexpectedly rejected" >&2
  exit 1
}

write_lock_fixture whitespace-bypass \
  'version = 4' \
  '[[package]]' \
  'name = "unsafe"' \
  'version = "1.0.0"' \
  '  source="git+https://example.invalid/unsafe"'
if lock_fixture_is_accepted "$LOCK_FIXTURE" >/dev/null 2>&1; then
  echo "lockfile source parser accepted a whitespace-bypass Git source" >&2
  exit 1
fi

write_lock_fixture non-string-source \
  'version = 4' \
  '[[package]]' \
  'name = "unsafe"' \
  'version = "1.0.0"' \
  'source = 7'
if lock_fixture_is_accepted "$LOCK_FIXTURE" >/dev/null 2>&1; then
  echo "lockfile source parser accepted a non-string source" >&2
  exit 1
fi

sed -n '/^deploy () {/,/^}/p' "$RELEASE_MANAGER" \
  | grep -F 'validate_lockfile_sources "$SOURCE_MUD/Cargo.lock"' >/dev/null || {
    echo "release build no longer enforces the parsed lockfile source boundary" >&2
    exit 1
  }

PYTHON_SHADOW="$CHECK_ROOT/python-shadow"
install -d -m 0755 "$PYTHON_SHADOW/urllib"
printf '%s\n' 'raise RuntimeError("cwd shadow module executed")' \
  >"$PYTHON_SHADOW/ipaddress.py"
printf '%s\n' 'raise RuntimeError("cwd shadow module executed")' \
  >"$PYTHON_SHADOW/pathlib.py"
printf '%s\n' 'raise RuntimeError("cwd shadow package executed")' \
  >"$PYTHON_SHADOW/urllib/__init__.py"

python_cwd_shadow_is_ignored() (
  source "$RELEASE_MANAGER"
  cd "$PYTHON_SHADOW"
  [ "$(normalize_database_endpoint DB.Example.COM. 03306)" = $'db.example.com\n3306' ] \
    || return 1
  [ "$(parse_backup_config_endpoint "$canonical_fixture"; \
       printf '%s\t%s' "$BACKUP_ENDPOINT_HOST" "$BACKUP_ENDPOINT_PORT")" \
      = $'127.0.0.1\t3306' ] || return 1
  [ "$(parse_database_url_identity \
       'mysql://test_user:test_pass@127.0.0.1:3306/deltamud')" \
      = $'dGVzdF91c2Vy\ndGVzdF9wYXNz\nMTI3LjAuMC4x\nMzMwNg==\nZGVsdGFtdWQ=' ]
)
python_cwd_shadow_is_ignored || {
  echo "release manager allowed its caller CWD to shadow a Python standard-library module" >&2
  exit 1
}

MARKER_ATOMIC_ROOT="$CHECK_ROOT/marker-atomicity"
install -d -m 0700 "$MARKER_ATOMIC_ROOT"
printf '%s\n' \
  'format=deltamud-activation-v2' \
  'state=maintenance' \
  'attempt=blocked' \
  'target=none' \
  'binary_sha256=none' \
  'boot_id=none' \
  'manager_pid=0' \
  'manager_starttime=0' \
  'backup_manifest=none' \
  'content_rollback_manifest=none' >"$MARKER_ATOMIC_ROOT/pending"
chmod 0600 "$MARKER_ATOMIC_ROOT/pending"
cp -- "$MARKER_ATOMIC_ROOT/pending" "$MARKER_ATOMIC_ROOT/original"

marker_pre_rename_failure_preserves_recovery_record() (
  source "$RELEASE_MANAGER"
  ACTIVATION_ROOT="$MARKER_ATOMIC_ROOT"
  ACTIVATION_MARKER="$MARKER_ATOMIC_ROOT/pending"
  ACTIVATION_MARKER_TEMP=
  mv() { return 1; }
  if publish_activation_marker_fields pending consumed none none none 0 0 none none \
      2>/dev/null; then
    return 1
  fi
  cmp -s -- "$MARKER_ATOMIC_ROOT/original" "$ACTIVATION_MARKER"
)
marker_pre_rename_failure_preserves_recovery_record || {
  echo "a pre-rename marker publication failure damaged the recovery journal" >&2
  exit 1
}
if grep -F '>"$ACTIVATION_MARKER"' "$RELEASE_MANAGER" >/dev/null; then
  echo "release manager directly truncates the sole activation marker" >&2
  exit 1
fi

/usr/bin/python3 -I - "$RELEASE_MANAGER" <<'PY'
import sys
from pathlib import Path


def shell_function(lines: list[str], name: str) -> list[str]:
    header = f"{name} () {{"
    starts = [index for index, line in enumerate(lines) if line == header]
    if len(starts) != 1:
        raise SystemExit(f"release manager must define {name} exactly once")
    start = starts[0]
    for end in range(start + 1, len(lines)):
        if lines[end] == "}":
            return lines[start : end + 1]
    raise SystemExit(f"release manager has an unterminated {name} function")


gate = [
    '  if [ "$activation_mode" = post-migration ] || [ "$content_manifest" != none ] \\',
    '      || [ "$content_transition" -eq 1 ]; then',
    '    validate_content_rollback_approval "$target" || return 1',
    '    [ "$content_manifest" = "$CONTENT_APPROVAL_MANIFEST" ] || {',
    '      echo "activation content rollback journal no longer matches the reviewed approval" >&2',
    '      return 1',
    '    }',
    '  fi',
]
publish = '  if ! write_activation_marker pending "$target" "$recovery_manifest" "$content_manifest"; then'


def has_immediate_migration_gate(body: list[str]) -> bool:
    matches = [index for index, line in enumerate(body) if line == publish]
    if len(matches) != 1:
        return False
    publish_index = matches[0]
    return body[publish_index - len(gate) : publish_index] == gate


source_lines = Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()
python_calls = [line for line in source_lines if "/usr/bin/python3" in line]
if not python_calls or any("/usr/bin/python3 -I " not in line for line in python_calls):
    raise SystemExit("every trusted-manager Python call must use isolated mode")
activation = shell_function(source_lines, "activate_release")
if not has_immediate_migration_gate(activation):
    raise SystemExit(
        "post-migration activation lacks an unconditional live-content gate "
        "immediately before pending/current publication"
    )

# Negative controls prove the structural assertion rejects the former
# content-transition-only gate and any command inserted after validation.
publish_index = activation.index(publish)
gate_start = publish_index - len(gate)
conditional_only = activation.copy()
conditional_only[gate_start] = '  if [ "$content_transition" -eq 1 ]; then'
if has_immediate_migration_gate(conditional_only):
    raise SystemExit("activation-boundary check accepted a conditional-only gate")
missing_validation = activation.copy()
del missing_validation[
    gate_start + gate.index('    validate_content_rollback_approval "$target" || return 1')
]
if has_immediate_migration_gate(missing_validation):
    raise SystemExit("activation-boundary check accepted a missing live-content validation")
not_immediate = activation.copy()
not_immediate.insert(publish_index, "  : # unsafe intervening activation work")
if has_immediate_migration_gate(not_immediate):
    raise SystemExit("activation-boundary check accepted a non-immediate gate")

migration_common = shell_function(source_lines, "migrate_activate_common")
if not any(
    'activate_release "$target" post-migration "$BACKUP_MANIFEST"' in line
    for line in migration_common
):
    raise SystemExit("migration common path no longer selects post-migration activation")
for wrapper in ("migrate_activate", "initialize_migrate_activate"):
    if not any("migrate_activate_common" in line for line in shell_function(source_lines, wrapper)):
        raise SystemExit(f"{wrapper} no longer uses the gated migration common path")
PY

echo "systemd units, release-manager pins, database/lockfile fixtures, and activation-boundary checks passed"
