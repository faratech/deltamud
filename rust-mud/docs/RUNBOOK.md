# DeltaMUD (Rust) — Runbook

Operational reference for running the Rust DeltaMUD server. For architecture,
see `../CLAUDE.md`; for C-compatibility caveats, see `../COMPATIBILITY.md`.

## Boot

```bash
cd /web/deltamud/rust-mud
cargo build --release            # ~1.5 min from clean; LTO+thin, panic=unwind

# Dev / test boot (in-memory mock DB — no MySQL needed):
MUD_MOCK_DB=true MUD_PORT=4000 MUD_LIB_PATH=/web/deltamud/lib ./target/release/deltamud

# Production-style boot (real MySQL, 83-column player_main):
DATABASE_URL=mysql://user@127.0.0.1:3306/deltamud MUD_PORT=4000 \
  MUD_LIB_PATH=/web/deltamud/lib ./target/release/deltamud
```

Environment (all read in `config.rs` / `main.rs`):

| Variable | Default | Notes |
|---|---|---|
| `MUD_MOCK_DB` | unset (MySQL) | `true` = in-memory MockDatabase; state dies with the process (copyover re-seeds from its validated snapshot) |
| `MUD_PORT` | 4000 | listen port |
| `MUD_LIB_PATH` | `./lib` | world/data dir — **must be the shared C lib** for real play |
| `MUD_METRICS_PORT` | disabled | Prometheus `/metrics`, `/health`, `/api/who`. **Never 9200/9201 — Elasticsearch owns them on this box; use e.g. 19666** |
| `MUD_METRICS_BIND` | `127.0.0.1` | Concrete metrics-listener IP. Keep loopback unless a firewall/reverse proxy restricts access; use `0.0.0.0` only as an explicit exposure decision. |
| `MUD_DB_TIMEOUT_SECS` | 5 | hard application-boundary timeout for every DB operation |
| `MUD_MAX_CONN` | 256 | accept-loop semaphore |
| `MUD_REVERSE_DNS` | true | bounded PTR lookup plus forward confirmation; false/0 disables hostname identity |
| `MUD_REVERSE_DNS_TIMEOUT_MS` | 1000 | whole hostname-resolution deadline; falls back to canonical peer IP |
| `MUD_REVERSE_DNS_MAX_INFLIGHT` | 16 | cap for uncancellable libc resolver calls |
| `MUD_CONN_BURST` / `MUD_CONN_WINDOW_MS` | 10 / 1000 | per-IP connect rate limit |
| `MUD_RNG_SEED` | time | pins the Lehmer PRNG — identical zone prime / combat for golden tests |
| `MUD_NO_SPECIALS` (or argv `-s`) | off | C-compatible no-specials mode (`-q` is NOT no-specials) |
| `DATABASE_URL` | `mysql://root:password@localhost/deltamud` | real-DB boot |
| `MUD_ENFORCE_MULTIPLAY` | off | makes `check_multiplaying` enforce in dev too |
| `MUD_CFORMAT_FILES` | off | selects exact C persistence for new/ambiguous runtime files; detected existing C/Rust formats are always preserved |
| `MUD_COMPAT_MODE` | off | enables registered C-compatibility behavior |
| `MUD_PT_MARKABLE` | off | enables C player-thief marking behavior |
| `MUD_WWW_WHO` / `MUD_WWW_WHO_DIR` | off / `./www` | `MUD_WWW_WHO=1` enables who2html output |
| `MUD_AUTOREBOOT` | off | `MUD_AUTOREBOOT=1` enables the scheduled reboot clock |

`MUD_LIB_PATH` is **load-bearing**: rent files (`plrobjs/`), aliases
(`plralias/`), `copyover.dat`, `etc/date_record` all resolve under it. Two
servers must never share a lib dir (crash-save/rent writes race).

## Copyover

`copyover` (immortal) first waits for prior saves, durably saves every playing
character, crash-saves objects and aliases, and only then prepares fd
inheritance and execs the binary with `--copyover <port> <listener_fd>`.

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

## Backup / restore

- **State that matters with real MySQL**: the database itself (players,
  affects, skills) + the lib dir's RUNTIME parts: `plrobjs/` (rent/inventory),
  `plralias/` (alias sidecars), `etc/date_record` (mud calendar), boards/mail
  files, `etc/clans.dat`.
- `date_record` is 12 bytes (year/month/day, native-endian i32s). Delete it to
  reset the calendar (boot warns `SYSERR: File etc/date_record not found` —
  that is exactly this, and self-inflicted in dev).
- Mock-DB boots: only the lib files matter; the DB evaporates on exit.

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
  heartbeat tick micros (+ max); players/mobs/objs gauges.
- `GET /health` — `ok` + player count. Liveness probe.
- `GET /api/who` — who-list JSON `{count, players:[{name, level, race, class,
  immortal, title}], generated_at}`; rebuilt once per second by the Game task.
- Same visibility rules as the web who list (invisible players excluded).
- The listener admits at most 32 concurrent exchanges. Read, write, shutdown,
  and whole-request work are each bounded by two seconds; excess connections
  are dropped immediately.

## Gates (run after gameplay-affecting changes)

| Gate | Command | Green means |
|---|---|---|
| Compile | `cargo check --all-targets` | every target type-checks |
| Formatting | `cargo fmt --all -- --check` | the complete Rust tree matches rustfmt |
| Unit/integration | `cargo test` | the complete suite passes in normal parallel mode |
| Serial race check | `cargo test -- --test-threads=1` | tests also pass without scheduler overlap |
| Lints | `cargo clippy --all-targets` | no clippy errors (the existing warning baseline remains visible) |
| Release | `cargo build --release` | the production profile builds with unwind enabled |
| Balance curve | `scripts/balance-check.sh` | no 5-level mob hole, no 10-level gear hole, no 5-level quest hole |
| MySQL persistence | `scripts/db-check.sh` | throwaway mariadbd :3307 round-trips the 83-column save/load |
| C-oracle parity | `scripts/parity-check.sh` | both drivers finish and the normalized C/Rust diff is empty |
| Isolated live smoke | `scripts/canary.sh --seconds 5 --players 1 --artifacts /tmp/deltamud-canary-smoke` | fresh mock DB/lib/ports; Playing, positive HP, combat, pulse, health, logs, and shutdown all prove green |

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
scripts/canary.sh --seconds 90 --players 3 --artifacts /tmp/deltamud-canary-release
scripts/canary.sh --seconds 1800 --players 8 --artifacts /tmp/deltamud-canary-extended
```

Retain each artifact directory with the corresponding commit SHA and deployment
record. Never use a production lib directory for a canary.

## Known behaviors / limits

- **Instances do not survive copyover** — runtime-only world additions are
  torn down by design; scheduled townsfolk/caravans re-derive from tables
  (stateless by design, `town_life.rs`).
- GMCP: Char.Vitals + Room.Info (with door/locked lists, player names, map
  coords), event-driven (pushed when state changes, drained per pulse /
  prompt). Room.Add/Remove deltas and Char.Items groups are not implemented.
- Stack-overflow-class aborts are NOT caught by the heartbeat's
  `catch_unwind` (only unwinding panics are). Keep `panic = "unwind"` in
  `[profile.release]` or command isolation stops working. If you see
  `fatal runtime error: stack overflow, aborting` in the log: capture the
  core (`ulimit -c unlimited`), `gdb ./target/release/deltamud core`, and
  read the repeating frame cycle — strip is DISABLED in the release profile
  for exactly this reason.
- `circle -c` (C oracle syntax check) exits 1 silently when it cannot reach
  MySQL; use the parity battery for a real C verdict.
