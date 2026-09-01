# DeltaMUD (Rust) — Runbook

Operational reference for running the Rust DeltaMUD server. For architecture,
see [`CLAUDE.md`](../CLAUDE.md); for C-compatibility caveats, see
[`COMPATIBILITY.md`](../COMPATIBILITY.md).

## Build and boot

`rust-toolchain.toml` pins the minimal Rust 1.98.0 toolchain with rustfmt and
clippy, and `Cargo.lock` pins the dependency graph. Keep both release inputs in
source control. Rustup selects the pinned toolchain automatically, and all
dependency-resolving commands must use `--locked`.

```bash
cd /web/deltamud/rust-mud
cargo build --release --locked    # thin LTO, symbols retained, panic=unwind

# Dev / test boot (in-memory mock DB — no MySQL needed):
MUD_MOCK_DB=true MUD_BIND=127.0.0.1 MUD_PORT=4000 \
  MUD_LIB_PATH=/web/deltamud/lib ./target/release/deltamud

# Local production-configuration smoke only. Production is launched by the
# trusted immutable-release manager described below, never from target/.
MUD_MOCK_DB=false \
DATABASE_URL='mysql://deltamud:<pw>@127.0.0.1:3306/deltamud' \
MUD_BIND=127.0.0.1 MUD_PORT=4000 MUD_LIB_PATH=/var/lib/deltamud/lib \
MUD_METRICS_BIND=127.0.0.1 MUD_METRICS_PORT=19595 \
MUD_EXEC_PATH=/opt/deltamud/current/bin/deltamud \
  ./target/release/deltamud
```

Environment (all read in `config.rs` / `main.rs`):

| Variable | Default | Notes |
|---|---|---|
| `MUD_MOCK_DB` | build-dependent | Debug/test defaults to mock; release defaults to real. Always set it explicitly. `true` is an in-memory MockDatabase whose state dies with a cold restart; `false` is MySQL. Invalid values fail configuration. |
| `DATABASE_URL` | none | Required and non-empty whenever the real backend is selected. There is no embedded credential or fallback URL. |
| `MUD_BIND` | `0.0.0.0` | Game-listener IPv4/IPv6 address. The systemd template uses `127.0.0.1`; expose it only through a separately reviewed edge or with an intentional public bind. Hostnames are rejected. |
| `MUD_PORT` | 4000 | listen port |
| `MUD_LIB_PATH` | `./lib` | World/data and writable runtime-state dir. Production uses a private copy such as `/var/lib/deltamud/lib`, populated from the authoritative content. |
| `MUD_METRICS_PORT` | disabled | Enables `/metrics`, `/live`, `/ready`, `/health`, and `/api/who`. Invalid values and listener bind failures abort startup. **Never 9200/9201 — Elasticsearch owns them on this box; use e.g. 19595.** |
| `MUD_METRICS_BIND` | `127.0.0.1` | Concrete metrics-listener IP; invalid values abort startup. Keep loopback unless a firewall/reverse proxy restricts access; use `0.0.0.0` only as an explicit exposure decision. |
| `MUD_DB_TIMEOUT_SECS` | 5 | hard application-boundary timeout for every DB operation |
| `MUD_EXEC_PATH` | current executable | Copyover target. Production should set the absolute release-aware path `/opt/deltamud/current/bin/deltamud`; it is resolved and validated at copyover time. |
| `MUD_MAX_CONN` | 256 | accept-loop semaphore |
| `MUD_REVERSE_DNS` | true | bounded PTR lookup plus forward confirmation; false/0 disables hostname identity |
| `MUD_REVERSE_DNS_TIMEOUT_MS` | 1000 | whole hostname-resolution deadline; falls back to canonical peer IP |
| `MUD_REVERSE_DNS_MAX_INFLIGHT` | 16 | cap for uncancellable libc resolver calls |
| `MUD_CONN_BURST` / `MUD_CONN_WINDOW_MS` | 10 / 1000 | per-IP connect rate limit |
| `MUD_RNG_SEED` | time | pins the Lehmer PRNG — identical zone prime / combat for golden tests |
| `MUD_NO_SPECIALS` (or argv `-s`) | off | C-compatible no-specials mode (`-q` is NOT no-specials) |
| `MUD_ENFORCE_MULTIPLAY` | off | makes `check_multiplaying` enforce in dev too |
| `MUD_CFORMAT_FILES` | off | selects exact C persistence for new/ambiguous runtime files; detected existing C/Rust formats are always preserved |
| `MUD_COMPAT_MODE` | off | enables registered C-compatibility behavior |
| `MUD_PT_MARKABLE` | off | enables C player-thief marking behavior |
| `MUD_WWW_WHO` / `MUD_WWW_WHO_DIR` | off / `./www` | `MUD_WWW_WHO=1` enables who2html output |
| `MUD_AUTOREBOOT` | off | `MUD_AUTOREBOOT=1` enables the scheduled reboot clock |

`MUD_LIB_PATH` is **load-bearing**: rent files (`plrobjs/`), aliases
(`plralias/`), `copyover.dat`, `etc/date_record` all resolve under it. Two
servers must never share a lib dir (crash-save/rent writes race).

## Schema migrations

Real-database startup is verification-only: it requires the complete ordered,
checksummed migration set and never creates or alters tables automatically. For
local development, back up the database, use a credential authorized for the
required DDL, and run the offline migration mode before the server starts:

```bash
cd /web/deltamud/rust-mud
MUD_MOCK_DB=false \
DATABASE_URL='mysql://deltamud:<pw>@127.0.0.1:3306/deltamud' \
  ./target/release/deltamud --migrate
```

Production must use the release manager's `migrate-activate <sha>` workflow
described below. It proves the OLC-safe service stop, creates and restore-tests a
fresh SQL + runtime-lib backup, runs the installed target binary through the
locked-down migration unit, and only then activates that exact binary. Do not
run a checkout binary against production.

The process takes the same database-scoped advisory lease that a normal live
server holds for its complete lifetime. It therefore refuses to migrate while
a server or another maintenance command is using that schema. After acquiring
exclusion it applies missing migrations, verifies their names and SHA-256
checksums through schema version 4, then exits without opening the game
listener. Verification also checks the complete table column sets, password
capacity, primary/unique identity indexes, a non-null case-insensitive
player-name column, and the player-level SQL type. A normal boot fails closed
with guidance to run `deltamud --migrate` if the schema is absent, stale, or
does not match the binary. Player-row loading independently rejects a missing,
mistyped, negative, or above-Implementor level before any integer conversion.
Startup also scans durable identities and rejects non-positive ids, names
outside the 2-20 ASCII-letter login domain (including accented or padded
imports), and invalid authorization levels. Repair those rows offline from a
backup rather than weakening the verifier.

## First Implementor bootstrap

Character creation never grants privilege implicitly: the first account is an
ordinary level-1 mortal even when it receives idnum 1.

For a local/development database only:

1. Run the schema migration.
2. Start the server, create the intended administrator through the normal nanny,
   and confirm that the character was saved.
3. Stop the server cleanly so the promotion is offline.
4. Promote that existing player once:

   ```bash
   MUD_MOCK_DB=false \
   DATABASE_URL='mysql://deltamud:<pw>@127.0.0.1:3306/deltamud' \
     ./target/release/deltamud --bootstrap-implementor Founder
   ```

The direct command requires MySQL, takes the shared runtime/maintenance advisory
lease, promotes the named durable player with one targeted SQL update, and
exits without starting the listener. It refuses invalid/non-player targets,
refuses to create a second effective Implementor, and fails if the live server
or another maintenance command still owns the database.

Production must never run that checkout binary or supply credentials on its
command line. After the initial installed release has migrated and activated,
create and save the intended player normally, then use the trusted manager:

```bash
sudo /usr/local/sbin/deltamud-release bootstrap-implementor \
  <40-character-sha> Founder --acknowledge-offline-authority-bootstrap
```

The manager requires that SHA to be the exact current immutable release,
performs and restore-tests a fresh stateful database/runtime backup, proves the
service has stopped cleanly, and invokes only that installed binary as the
unprivileged runtime account with a scrubbed environment. It leaves the service
stopped and prints the recovery manifest. Subsequent administration must be
authenticated in game. The database lease remains a fail-closed enforcement
backstop, not a substitute for the normal shutdown/save procedure.

A real-database server keeps that lease on a dedicated non-pooled MySQL session
and verifies ownership once per second with a two-second deadline. If the
session or ownership is lost, the process stops the game task immediately and
returns failure without attempting an unsafe DB-backed save. The mock backend
does not need a database lease, and both maintenance modes continue to reject
mock configuration. MySQL named locks are cooperative: direct SQL tools and the
legacy C server do not honor this protocol, so operators must still keep them
away from the live schema.

## Administrative authority

Player authority is the persisted tuple of level, trust, and four GCMD
bitvectors. Sensitive dispatch, disclosure, builder publication, snooping, and
administrative overrides resolve the exact player principal controlling the
active Playing descriptor; display level, a switched NPC body, forced input,
DG scripts, and descriptorless high-level NPCs do not confer staff authority.
An identity in the authority quarantine has no usable administrative authority.

Use `advance` for player promotion or demotion. Its asynchronous database work
compare-and-swaps the complete expected authority tuple, reads back an
indeterminate result, and changes the live character only after durable state
is known. A demotion installs the canonical grants for the resulting level.
`set level`, `set trust`, and `set cmd*` deliberately reject player targets so
they cannot bypass this path. If an update cannot be reconciled, preserve its
`AUDIT: authority update` log and repair the durable row before reconnecting or
otherwise clearing the incident; do not edit only the live process.

Copyover, command-requested shutdown/reboot, and pfileclean are delayed effects.
Each captures the initiating body, principal, descriptor, idnum, and required
grant, then revalidates that exact session after queued authority work drains.
A disconnect, body/principal change, demotion, quarantine, or grant revocation
cancels the request before files, SQL, broadcasts, control flags, or exec are
touched. Scheduled automatic reboot is a separate system-origin request.

## Password storage and upgrades

New and changed passwords are stored as salted Argon2id PHC strings using the
RustCrypto defaults and operating-system randomness. Historical DES (including
the old truncated form), `$5$`/`$6$` SHA-crypt, and bare lowercase SHA-256
records remain verifiable. After a successful legacy or below-policy Argon2id
login, the server computes a current Argon2id hash and compare-and-swaps only
the credential column; a concurrent password change wins and is never replaced
by the login upgrade. Ordinary character saves never read or write `pwd`.
Failed logins never trigger a rewrite. Migration 4 ensures `player_main.pwd`
has the 255-byte capacity required by PHC strings.

Verification rejects plaintext input above 64 bytes before invoking any KDF.
Imported Argon2id records are accepted only up to 65536 KiB memory, 4 passes,
4 lanes, and 64-byte salt/output fields; SHA-crypt accepts at most 100000
rounds. Records above those online-work caps deliberately fail authentication.
Reset such an account through an authenticated administrator or approved
offline credential-reset procedure; do not raise the server caps temporarily.
An Argon2id record whose cost dimensions all meet the defaults remains current.
A mixed record with any below-policy dimension is normalized to the current
defaults on upgrade.

Argon2id work runs in bounded blocking workers rather than on the single-owner
game loop. Character creation computes the final hash once and passes that hash
to the collision-safe insert; it does not hash first for validation and again
for storage. The terminal `unlock` command likewise verifies asynchronously and
clears lockout only after the same live session is revalidated.

## Copyover

`copyover` (immortal) first waits for prior saves, durably saves every playing
character, crash-saves objects and aliases, and only then prepares fd
inheritance and execs the binary with `--copyover <port> <listener_fd>`.

Production must set `MUD_EXEC_PATH` to the release-aware current symlink. At the
moment copyover runs, the path must be absolute and must canonicalize to a
regular file executable by the service account. The configured and canonical
binary, and every ancestor directory in both path chains, must be root-owned
and must not be group/world-writable. Validation failure aborts before the live
process is replaced. Development retains `current_exe()` as a compatibility
fallback when the variable is absent. The systemd/release scaffold uses:

```text
MUD_EXEC_PATH=/opt/deltamud/current/bin/deltamud
```

`copyover.dat` is the sole recovery snapshot. It is a versioned JSON envelope
with an explicit record count, completion flag, and SHA-256 payload checksum.
It contains each connection and a typed character snapshot, so strings such as
titles are escaped rather than relying on delimiters. Publication is atomic:
sibling temporary file, checked write/flush/fsync, rename, then parent-directory
fsync.

Recovery validates the entire snapshot—including version, completion, count,
checksum, listener/client fds, names, enum fields, and duplicate ids/names/fds—
and re-seeds only an ephemeral mock DB before unlinking the snapshot or adopting
any socket. A pre-exec persistence or snapshot error aborts copyover and leaves
the running process and existing sockets intact. A failed recovery keeps the
snapshot as forensic evidence; archive it before removing it and returning to a
normal cold boot.

Recovered descriptors deliberately start with empty GMCP capability state. The
new process sends `WONT GMCP` followed by `WILL GMCP`, so the client and server
renegotiate instead of trusting stale pre-exec state.

The recovery snapshot also deliberately excludes password hashes. A character
whose terminal was AFK-locked at copyover therefore cannot use `unlock` in the
recovered session; the command fails closed and asks the player to reconnect,
which repopulates the authenticated session hash through the normal nanny.

## Durable OLC saves

OLC file writes use a unique sibling temporary file, checked write and flush,
file `fsync`, atomic rename, and (on Unix) parent-directory `fsync`. A failure
before rename leaves the previous world file in place and removes the temporary
file best-effort. A parent-directory `fsync` failure occurs after rename, so the
new file may already be visible while crash durability remains indeterminate;
the save remains dirty and must be retried. Disk-first editors (mobile, zone,
shop, action, help, and trigger) publish edited live state only after their disk
representation succeeds. REDIT and OEDIT retain the C two-stage workflow:
accepting an edit publishes it to memory and marks the zone save list; `olc
save` performs the durable file write. Room/object/mobile/zone/shop central
writers remove their save-list item only after durable replacement.

Manual `olc ... save`, graceful shutdown, scheduled auto-reboot, and copyover
attempt every outstanding save-list entry and report partial success. Any
failed item remains listed, emits `SYSERR: OLC`, and blocks the success message,
process exit, or exec. Treat that warning as unresolved builder work: correct
the filesystem/permission/capacity problem, retry the save, and run the `olc`
save-info command before restart or deployment.

Creating a brand-new zone remains a multi-file operation, but it is guarded by
a versioned marker under `world/.new-zone-transactions/`. The marker is made
durable before any of the six component/index pairs can publish. Until all
twelve files are confirmed exactly and the marker is durably removed, boot
hides every indexed component for that zone and shutdown, auto-reboot, and
copyover stay blocked. After interruption or a partial publication, retry the
same `zedit new <zone>` as an Implementor; the retry is idempotent and refuses
an intervening or unreadable index state. A partial publication cannot be
discarded. Back up the world tree before recovery and retain the marker as
evidence until the exact retry succeeds.

Editor authority is also rechecked at publication, not only at editor entry.
An open edit cannot publish after its exact descriptor/principal changes, its
OLC grant or trust is revoked, it is quarantined, or its current zone ownership
is removed. Builder-list tokens are exact case-insensitive player names; a name
referenced by any current builder ACL is reserved from new-character reuse.

## Backup / restore

- **State that matters with real MySQL**: the database itself (players,
  affects, skills) + the lib dir's RUNTIME parts: `plrobjs/` (rent/inventory),
  `plralias/` (alias sidecars), `etc/date_record` (mud calendar), boards/mail
  files, `etc/clans.dat`.
- `date_record` is 12 bytes (year/month/day, native-endian i32s). Delete it to
  reset the calendar (boot warns `SYSERR: File etc/date_record not found` —
  that is exactly this, and self-inflicted in dev).
- Mock-DB boots: only the lib files matter; the DB evaporates on exit.

Production release backups are created only while `deltamud.service` is
verified inactive. The trusted manager writes a root-only SQL dump, a
numeric-owner/xattr/ACL runtime-lib archive, and a mode-0600 manifest beneath
`/var/backups/deltamud`. It imports the dump into a temporary
`deltamud_restorecheck_*` database and refuses to publish the manifest unless
the restored typed table/view, routine, trigger, and event names and details
exactly match the source inventory;
only the explicitly acknowledged initialization workflow accepts zero objects.
The manifest binds the target revision, canonical typed-object inventory hash,
runtime database identity, environment-file hash, artifact paths and SHA-256
hashes, completion time, and successful restore drill. `content-approve` accepts
only one of these fresh fixed-root manifests; an arbitrary nonempty file is not
backup evidence.

Player mail deletion rewrites the complete mail store through a unique sibling
temporary file, file and parent-directory sync, and atomic replacement. A
failed multi-block deletion leaves the original message readable and does not
publish a partial chain. Preserve the old store and the `SYSERR` log if a
post-rename directory sync makes durability indeterminate.

## Character self-deletion

Self-deletion is ordered and fail closed. The player row must first be saved
with the durable deletion tombstone; if that database save fails, neither the
rent file nor alias sidecar is removed. After the tombstone succeeds, missing
sidecars count as already clean. Any other sidecar error is logged with an
`AUDIT:` prefix and the player is explicitly told cleanup is incomplete instead
of receiving a false success message. Preserve that audit entry and repair the
named sidecar before the next pfile-clean cycle.

## Metrics / health

- `GET /metrics` — Prometheus: pulse, uptime, connection, and command counters;
  heartbeat tick micros (+ max); readiness/heartbeat age; players/mobs/objs
  gauges.
- `GET /live` — always `200 live` while the small HTTP task can answer. Use as
  process liveness only; it does not prove that world boot or the Game heartbeat
  is healthy.
- `GET /ready` — `200 ready` only after database/schema/world boot has completed,
  the heartbeat has started, and its last pulse is no more than two seconds old;
  otherwise `503` with `boot incomplete`, `heartbeat not started`, or
  `heartbeat stale`. Use this for release activation and traffic admission.
- `GET /health` — backwards-compatible always-200 status with current player
  count. Do not use it as a readiness probe.
- `GET /api/who` — who-list JSON `{count, players:[{name, level, race, class,
  immortal, title}], generated_at}`; rebuilt once per second by the Game task.
- Same visibility rules as the web who list (invisible players excluded).
- The listener admits at most 32 concurrent exchanges. Read, write, shutdown,
  and whole-request work are each bounded by two seconds; excess connections
  are dropped immediately.

## Gates (run after gameplay-affecting changes)

| Gate | Command | Green means |
|---|---|---|
| Compile | `cargo check --all-targets --locked` | every target type-checks against the committed lockfile |
| Formatting | `cargo fmt --all -- --check` | the complete Rust tree matches rustfmt |
| Unit/integration | `cargo test --locked` | the complete suite passes in normal parallel mode |
| Serial race check | `cargo test --locked -- --test-threads=1` | tests also pass without scheduler overlap |
| Lints | `scripts/clippy-check.sh` | `-D warnings` plus the explicit inherited-port lint baseline; new lint categories fail |
| Dependency audit | `cargo audit --deny warnings --file Cargo.lock` | no known advisories, unmaintained/yanked packages, or audit warnings |
| Release | `cargo build --release --locked` | the production profile builds with unwind enabled |
| Balance curve | `scripts/balance-check.sh` | no 5-level mob hole, no 10-level gear hole, no 5-level quest hole |
| MySQL persistence | Run `scripts/db-check.sh` as an unprivileged user | throwaway unprivileged mariadbd with TCP disabled and a private Unix socket verifies migrations and 83-column save/load; checkout-controlled SQL and tests never cross a root boundary |
| Offline playthrough tests | `PYTHONDONTWRITEBYTECODE=1 python3 scripts/playthrough_test.py -v` | fragmented Telnet, fail-closed milestones, credential gating, and transcript redaction pass |
| C-oracle parity | Build with `cargo build --release --locked` as an unprivileged user, then run `RUST_BIN="$PWD/target/release/deltamud" scripts/parity-check.sh` | a user namespace maps only the invoking host identity; private network/PID/IPC/mount namespaces, a recursively read-only host view, a read-only staged input tree, and capability-free oracle processes contain the throwaway MariaDB/C/Rust run. The diff strips only Telnet, ANSI, CR, and blank-line transport noise. The gate refuses host-root execution. |
| Isolated live smoke | `scripts/canary.sh --seconds 5 --players 1 --artifacts /var/tmp/deltamud-canary-smoke` | fresh mock DB/lib/ports; Playing, positive HP, combat, pulse, readiness, logs, and shutdown all prove green |

`lib/world/**` edits additionally need the C oracle to boot the same lib (the
parity battery does this in a private netns with its own MariaDB; never run
the C binary against production MySQL).

The repository-root workflow `.github/workflows/rust-mud-ci.yml` runs the
bounded smoke and database transaction gate on pushes and pull requests, and
archives canary logs, metrics, and client transcripts. It also verifies that an
injected failing test plus `kill-server`, `freeze-pulses`, and `driver` canary
controls all return nonzero.

Before a release, run a three-player 90-second canary. On the scheduled extended
cadence, exercise all eight supported clients for 30 minutes:

```bash
scripts/canary.sh --seconds 90 --players 3 --artifacts /var/tmp/deltamud-canary-release
scripts/canary.sh --seconds 1800 --players 8 --artifacts /var/tmp/deltamud-canary-extended
```

Each explicit canary artifact path must not already exist. The runner creates
it mode 0700 and refuses to mix a new result with stale files or follow a
destination symlink. With no `--artifacts`, it creates and prints a unique
private, disk-backed directory under `/var/tmp`.

For a semantic new-player journey against an already isolated running server:

```bash
scripts/playthrough.py --host 127.0.0.1 --port 4000
```

It proves character creation, school/town travel, look/help/score, a bounded
quest/shop/combat probe, clean quit, and reconnect. Copyover is disabled unless
explicitly requested with an existing Implementor name and a password supplied
through the documented environment variable. This smoke journey does not by
itself prove quest completion, economy balance, or the full #368 epic.

With no `--artifacts` argument, the driver atomically creates a unique private
`/var/tmp/deltamud-playthrough-<timestamp>-<pid>` directory and prints its path. An
explicit artifact path must not already exist: the driver creates it mode 0700
and creates `transcript.txt` and `result.json` exclusively without following
symlinks. Never pre-create or reuse the directory. Timeout arguments must be
finite and positive; connect, step, copyover, and overall timeouts are capped at
60, 120, 300, and 3600 seconds respectively.

Retain each ad-hoc artifact directory with the corresponding commit SHA and
deployment record. The trusted release manager instead moves successful canary
evidence into the immutable installed release, records its SHA-256 in
`CANARY_MANIFEST`, and makes the complete tree root-owned and non-writable.
Never use a production lib directory for a canary.

## systemd and immutable releases

The repository contains a deployment scaffold; it does not imply that this host
has installed or enabled it:

- `deploy/systemd/deltamud.service` runs an unprivileged `deltamud` user from
  `/var/lib/deltamud`, reads `/etc/deltamud/deltamud.env`, treats `/opt/deltamud`
  as read-only, and limits game-writable state to `/var/lib/deltamud`. Its two
  fixed trusted-manager helpers may also write the `root:root` mode-0700
  `/etc/deltamud/activation` control directory; the game account cannot traverse
  that directory.
  DeltaMUD exits 75 after a durably saved scheduled reboot, bare `shutdown`, or
  `shutdown reboot/now`; the unit restarts that status. `shutdown die/pause`,
  SIGTERM, and Ctrl-C exit 0 and remain stopped. Unexpected failures still use
  nonzero failure status and follow `Restart=on-failure`. `SendSIGKILL=no` is
  intentional: if authority reconciliation or OLC/player persistence refuses a
  stop, systemd must not turn the recoverable refusal into data-losing SIGKILL.
  A failed `systemctl stop` therefore means the server may still be online and
  must be investigated, not killed.
- `deploy/systemd/deltamud-migrate@.service` runs only an installed release's
  `--migrate` mode as `deltamud`. The release manager verifies the installed
  unit's canonical hash, absence of drop-ins, and effective ExecStart, identity,
  environment and hardening before allowing it to touch the schema. A durable
  `migrating` activation journal and the shared database lease prevent the main
  service from starting concurrently; the units do not rely on a racy systemd
  conflict relationship. Migration-unit startup has no systemd timeout: the
  manager retains its exclusive lock, reports progress every 30 seconds, and
  waits for two identical terminal unit snapshots. It never kills an in-flight
  DDL operation or reports the synchronous transaction complete while the unit
  is still running.
- `deploy/systemd/deltamud.env.example` selects real MySQL explicitly, binds the
  game and metrics listeners to loopback, and points copyover at
  `/opt/deltamud/current/bin/deltamud`. Install the populated file as
  `root:deltamud` mode exactly `0640`; use unique unquoted, whitespace-free
  literal assignments without whitespace, quotes, or backslashes and never
  commit its credential. Percent-encode reserved credential bytes. The manager requires exact
  production values for real DB mode, `/var/lib/deltamud/lib`, the current
  immutable copyover path, and the loopback `127.0.0.1:19595` readiness endpoint,
  and rejects dynamic-loader/interpreter injection variables.
- `deploy/systemd/backup.cnf.example` is a separate root-only MariaDB backup
  credential. Install it as `/etc/deltamud/backup.cnf`, `root:root` mode `0600`.
  The file may contain exactly one `[client]` group with unique `protocol`,
  `host`, `port`, `user`, and `password` entries. `protocol` must be exactly
  `tcp`; host must be a valid DNS name or IP literal and port must be in
  `1..65535`. The manager canonicalizes IP literals, DNS case/trailing dot, and
  decimal port spelling, then requires the resulting host and port to match the
  runtime `DATABASE_URL` exactly. It never prints the backup or runtime
  password. The account needs dump access to `deltamud` plus narrowly scoped
  create/drop rights for `deltamud_restorecheck_*` restore drills.
- `deploy/systemd/deltamud.tmpfiles.conf` declares the configuration, root-only
  activation-control, state, and immutable-release directories with narrow
  ownership and modes.
- `scripts/release.sh` is source for the trusted control plane, not a command to
  run with `sudo` from `/web`. The checkout and Git metadata are writable by the
  web account. Install the reviewed script from an independently verified
  artifact at `/usr/local/sbin/deltamud-release`, `root:root` mode `0755`; that
  path and every parent must be root-controlled. The manager refuses privileged
  execution from the checkout.

Provision a dedicated `deltamud-build` account with a locked password,
`nologin`/`false` shell, a same-named private primary group, and no supplementary
groups. No other account may use that group as its primary group. Provision the
complete Rust 1.98.0 toolchain (`cargo`, `rustc`, `rustfmt`, `clippy-driver`)
plus exactly `cargo-audit 0.22.2` as a
root-owned, non-writable, link-free tree at
`/opt/deltamud/toolchains/1.98.0`. Install root-owned, non-writable bubblewrap at
`/usr/bin/bwrap`; canaries and every code-executing Cargo phase use it for a
minimal filesystem plus private network, PID, IPC, device, and user namespaces.
The exact crates.io-only lockfile is fetched first; formatting, checking, tests,
Clippy, and the release build then run offline. The manager gives builds an
ephemeral HOME, Cargo cache, disk-backed temp directory and target directory,
clears supplementary groups and capabilities, and refuses any process left
under the build UID.

Before the first activation, create the service identities, install the main and
migration units plus tmpfiles configuration, populate the two protected
credential files, provision `/var/lib/deltamud/lib`, install the pinned toolchain
and trusted manager, review the edge network path, then reload systemd:

```bash
sudo systemctl daemon-reload
sudo systemctl enable deltamud.service
```

Run every trusted-manager command below through the installed root-controlled
path with `sudo`; never prefix a checkout script, build, DB gate, or parity gate
with `sudo`. The manager accepts a literal, locally available full commit SHA;
the writable checkout state is not a release input. Git, formatting, checks,
parallel and serial tests, Clippy, dependency audit, release build and canary all
run without privilege against a fresh exact-SHA object snapshot. The tested
binary is copied into a root-owned stage before the canary. Releases include the
binary, documentation, lockfile, frozen canary evidence, and a committed
`content/lib` snapshot with a canonical type/path/mode/content digest. Links,
special nodes, and hard-linked files are rejected; empty directories are bound
into the digest. That snapshot is never blindly copied over the mutable runtime
tree.

For an existing stateful database and any commit whose `lib/**` digest changed,
use the offline content workflow:

```bash
sudo /usr/local/sbin/deltamud-release install <40-character-sha>
sudo /usr/local/sbin/deltamud-release backup <40-character-sha>
# While the service remains stopped, review release content/lib against
# /var/lib/deltamud/lib and deliberately merge the intended static/world edits.
# Preserve rent, aliases, player data, mail, boards, clans, calendar, houses,
# OLC recovery markers, and every other live-only or newer builder-authored file.
sudo /usr/local/sbin/deltamud-release content-approve <40-character-sha> \
  /var/backups/deltamud/backup.<timestamp>.<sha>.<suffix>/manifest \
  --acknowledge-reviewed-runtime-merge
# If no schema migration is needed, consume this evidence only through:
sudo /usr/local/sbin/deltamud-release activation-resolve \
  <40-character-sha> --acknowledge-reconciled-state
```

`content-approve` records the release content digest, exact verified backup and
the reconciled live-tree digest plus the stopped service generation in a
root-controlled marker; it does not perform the merge. Journal-bound,
content-changing, and post-migration activation paths recompute that digest and
service generation before consuming the approval. `backup` publishes a durable
`maintenance` journal before stopping the service and keeps it through
reconciliation, so direct starts and host reboots cannot expose a partially
merged tree. A code-only release with the already approved content digest may
use the one-step deployment:

```bash
sudo /usr/local/sbin/deltamud-release deploy <40-character-sha> \
  --acknowledge-no-state-restore
```

If the installed binary expects a new schema, first complete the exact-revision
backup and `content-approve` workflow above. That workflow leaves a durable
maintenance journal, so keep the service stopped and resume that exact journal
explicitly:

```bash
sudo /usr/local/sbin/deltamud-release migrate-activate <40-character-sha> \
  --acknowledge-reconciled-state
```

The bare form is only for initiating a new maintenance journal when none
exists:

```bash
sudo /usr/local/sbin/deltamud-release migrate-activate <40-character-sha>
```

It stops the service and creates a fresh backup, then deliberately refuses to
run the migration until the live content has been reconciled and approved.
Finish that interrupted flow with `content-approve`, then rerun the acknowledged
form above.

This creates another same-operation backup and successful temporary restore
drill, proves the backup credential and runtime `DATABASE_URL` name the same
normalized TCP host and port and query the same live server/schema identity,
runs the target migration unit to a terminal success, atomically selects the
release, starts it, and verifies that the exact systemd MainPID both
owns the `127.0.0.1:19595` listening socket and returns `/ready` before and after
the identity check. Signals are deferred across the
migration and activation transaction. If a post-migration or content-changing
activation fails, the old link is restored but the service remains stopped:
restore/reconcile the recorded database and content backup before starting any
old binary.

For an already compatible installed release, manual activation makes the same
no-state-restore assertion as a binary-only rollback:

```bash
sudo /usr/local/sbin/deltamud-release activate <40-character-sha> \
  --acknowledge-no-state-restore
sudo /usr/local/sbin/deltamud-release rollback <40-character-sha> \
  --acknowledge-no-state-restore
```

These commands refuse every pre-existing activation or maintenance journal;
finish any such transaction through `activation-resolve`. The rollback alias
is retained for operator intent but has the same guarded activation path.
They are schema-compatible-only. Never restore a database or lib archive
before invoking it: an ordinary replacement failure may safely restart the
previously selected newer binary. For a state-changing rollback, first run
`backup <old-sha>` to publish the stopped maintenance journal and checkpoint
the current state, restore/reconcile the intended old schema and runtime tree,
run `content-approve` when the content digest differs, and finish only through
`activation-resolve <old-sha> --acknowledge-reconciled-state`. A failed resolved
start remains stopped and retains both recovery artifacts.

For an already provisioned `deltamud` database that exists but contains zero
objects, the ordinary backup path deliberately refuses the empty inventory.
Create that empty database and its narrowly scoped credentials first, then use
the explicit initialization acknowledgements:

```bash
sudo /usr/local/sbin/deltamud-release install <40-character-sha>
sudo /usr/local/sbin/deltamud-release initialize-backup <40-character-sha> \
  --acknowledge-empty-database
# Reconcile /var/lib/deltamud/lib, then approve it using the printed manifest.
sudo /usr/local/sbin/deltamud-release content-approve <40-character-sha> \
  /var/backups/deltamud/backup.<timestamp>.<sha>.<suffix>/manifest \
  --acknowledge-reviewed-runtime-merge
sudo /usr/local/sbin/deltamud-release initialize-migrate-activate \
  <40-character-sha> --acknowledge-empty-database
```

The initializer still verifies the empty-state backup, database identity,
environment, migration result, exact process/socket readiness, and immutable
release. After creating the first ordinary player, use the trusted bootstrap
command documented above.

Activation publishes `maintenance` before the offline window, then proves the
old process and every real/effective runtime-UID process have stopped, so an old
copyover can never resolve a newly selected executable. A content-changing
activation records two distinct artifacts: the approved pre-merge rollback
manifest and a fresh, restore-tested pre-candidate checkpoint of the reconciled
state. It then durably advances to a single-attempt journal beneath
`/etc/deltamud/activation`, rollback metadata, and `/opt/deltamud/current`. The
unit's root-only `ExecCondition` consumes that one attempt; `ExecStartPost`
confirms the exact MainPID, immutable binary, listener ownership, and `/ready`
response in the journal before the manager durably removes it. Normal boots
apply the same process/socket readiness proof. A non-state-changing replacement failure restores the old links,
restarts only the validated old binary when necessary, and proves readiness.
Release directories are immutable deployment artifacts. Persistent SQL and
`/var/lib/deltamud/lib` remain outside them and always require their own verified
backup and rollback decision.

Never delete or edit an activation journal by hand. If a crash leaves a
`confirmed` journal while its exact target is still the ready MainPID, run:

```bash
sudo /usr/local/sbin/deltamud-release activation-recover
```

For `maintenance`, `pending`, `migrating`, or `blocked`, inspect the journal,
service and migration-unit state, and its recorded backup. Restore or reconcile
SQL and the runtime tree as required and leave the service fully stopped. A
terminal failed or ambiguous `migrating` journal may not activate that same
target. After restoring/reconciling its recorded backup, retry schema work with:

```bash
sudo /usr/local/sbin/deltamud-release migrate-activate \
  <40-character-sha> --acknowledge-reconciled-state
```

For an initialization retry, restore the recorded empty backup and use
`initialize-migrate-activate <sha> --acknowledge-empty-database`; the fresh
backup gate proves the database still has zero objects. Each retry first proves
the old migration unit is stably terminal, revokes its authorization, and
creates and journals a new restore-tested backup before authorizing one new
invocation.

For a clean completed migration, another unresolved activation, or an
explicitly reconciled alternate installed revision, close the ambiguity with:

```bash
sudo /usr/local/sbin/deltamud-release activation-resolve \
  <40-character-sha> --acknowledge-reconciled-state
```

For the same target, the resolver requires a consumed migration authorization
and clean terminal exit; it never treats a failed DDL attempt as permission to
boot that schema. It refuses a running/changing migration unit, creates and restore-tests
a fresh checkpoint of the explicitly reconciled state before retrying, and
retains evidence on failure. It may deliberately select a different installed
revision from the unresolved journal. If that target does not have a current,
exact content approval, the first resolve retargets the durable journal, records
the fresh checkpoint, leaves the service stopped, and prints the bound
`content-approve` command; approve the reconciled tree and repeat the resolve.
An interrupt received during a migration, stop, link switch, or start is
recorded and returned only after the critical transaction reaches a stable
success or recovery state.

## Known behaviors / limits

- **Never share one writable lib tree between processes.** The OLC publication
  mutex is process-local. The real-DB runtime lease prevents two cooperating
  Rust servers on one schema, but mock servers or servers using different
  databases can still race if pointed at the same `MUD_LIB_PATH`.
- **Instances do not survive copyover** — runtime-only world additions are
  torn down by design; scheduled townsfolk/caravans re-derive from tables
  (stateless by design, `town_life.rs`).
- GMCP is server-initiated (`WILL GMCP`) and activates only after the client
  replies `DO GMCP`. Bounded `Core.Hello` and
  `Core.Supports.Set/Add/Remove` messages populate per-descriptor capability
  state; refusal/disable clears it, and non-negotiated clients receive no GMCP
  payload. Outbound support remains event-driven `Char.Vitals` + `Room.Info`
  (including door/locked lists, player names, and map coordinates).
- Plain Telnet and one-shot MSSP remain supported. `Room.Add/Remove`,
  `Char.Items`, UTF-8 negotiation/input policy, NAWS, TTYPE/MTTS, and MCCP are
  intentionally deferred protocol work.
- Stack-overflow-class aborts are NOT caught by the heartbeat's
  `catch_unwind` (only unwinding panics are). Keep `panic = "unwind"` in
  `[profile.release]` or command isolation stops working. If you see
  `fatal runtime error: stack overflow, aborting` in the log: capture the
  core (`ulimit -c unlimited`), `gdb ./target/release/deltamud core`, and
  read the repeating frame cycle — strip is DISABLED in the release profile
  for exactly this reason.
- `circle -c` (C oracle syntax check) exits 1 silently when it cannot reach
  MySQL; use the parity battery for a real C verdict.
