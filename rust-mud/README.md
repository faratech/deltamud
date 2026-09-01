# DeltaMUD — Rust Edition

A broad Rust reimplementation of DeltaMUD (a CircleMUD 3.0 derivative), plus a layer of modern improvements. The C source at `/web/deltamud/src` is the authoritative reference; this port matches its output strings, world-file formats, numeric formulas, and database schema while replacing the C pointer graph with an idiomatic single-owner design. The 2026-08 parity program (GitHub issues #96-#347, epic #348) closed every confirmed fidelity gap from the 12-subsystem audit; the handful of places where the port deliberately repairs a C bug are registered in [`COMPATIBILITY.md`](COMPATIBILITY.md).

~83 modules / ~75k lines. Builds clean and boots against the original `lib/` world files.

> For development guidance (architecture deep-dive, conventions, gotchas), see [`CLAUDE.md`](CLAUDE.md).

## What's implemented

The Rust port has the broad DeltaMUD feature surface in place, including the static command table and major subsystems:

- **World**: rooms/objects/mobiles/zones/shops loaders (incl. `E` extra-descriptions, `A` applies, room `O` special exits, mob `X`-stats + espec, DG `T` triggers), zone resets with load-chance gating.
- **Gameplay**: combat (DeltaMUD `chance()`/`dam_multi()` from `utils.c` — not stock THAC0 — plus avoid/parry/dodge/riposte), magic/skills (spell_parser/magic/spells), classes/races/deities/languages, regen, affects, conditions, `WAIT_STATE` command lag.
- **Content & economy**: shops, clans, boards, mail, houses, quests, auction, arena, the castle/special procedures.
- **DG Scripts**: full VM (`script_driver` with depth + loop guards), mob/obj/wld trigger command sets, and fire-hooks (greet/command/speech/death/load/timer/random).
- **OLC**: redit / oedit / medit / zedit / sedit / aedit / hedit / trigedit, with central save dispatch, publication-time session/ACL revalidation, DG attachment editing, durable atomic replacement, and crash-recoverable new-zone publication gates.
- **Persistence**: 83-column `player_main` + `player_affects` + `player_skills` (MySQL), checksummed schema migrations, exact compare-and-swap authority updates, auto-detected C/Rust runtime files, and Argon2id passwords with legacy verification and login-time upgrade attempts; offline-player immortal ops use an async bridge.
- **Immortal tooling**: the full `act.wizard` command set, god-command (GCMD) permission bits, `can_edit_zone`, autowiz, on-disk syslog, the player-index table.

No open fidelity-gap issues remain (epic #348). Deliberate divergences — places where the C oracle is buggy and the port repairs it — are listed in the `COMPATIBILITY.md` divergence register. C-format runtime persistence files (plrobjs rent/crash, hcontrol, house objects, clans.dat, boards) are auto-detected and retain their detected format on atomic rewrite; `MUD_CFORMAT_FILES=true` selects C format only for new or intrinsically ambiguous empty files (#95).

### Modern improvements over the C version
- **Idiomatic Rust core**: a single-owner `GameState` (id-indexed `IndexMap`/`Vec` arenas, `Copy` ids instead of locked pointers) — deadlock-free, with async (Tokio) only at the socket edge. No `Arc<RwLock>` entity graph.
- **Seamless copyover**: hot-reboots a validated executable (`execv` + inherited socket fds) while keeping every playing connection attached — same user-visible result as the C MUD, with a versioned and checksummed recovery snapshot.
- **Crash isolation**: a panic in any command or heartbeat handler is contained (`catch_unwind`) and logged with a backtrace, instead of killing the server.
- **Modern client protocols**: server-initiated **GMCP** negotiation, bounded `Core.Hello` and `Core.Supports.Set/Add/Remove` capability state, `Char.Vitals`/`Room.Info` pushes, and one-shot **MSSP**, while retaining plain Telnet compatibility.
- **Observability**: optional Prometheus `/metrics`, process liveness `/live`, game-loop readiness `/ready`, compatibility `/health`, and `/api/who` endpoints, plus syslog rotation.
- **Hardening**: IP/hostname ban gates (bounded forward-confirmed reverse DNS), connection rate-limit + max-connections, direct authenticated-principal provenance for staff capabilities, and graceful SIGTERM/Ctrl-C shutdown with save-all.
- **Account security**: new and changed passwords use salted Argon2id PHC strings computed off the game loop; account creation hashes once, terminal unlock verifies asynchronously, and successful legacy logins use a compare-and-swap upgrade from supported DES, SHA-crypt, and bare SHA-256 records so a concurrent password change cannot be overwritten.

## Building & running

The repository pins Rust **1.98.0** in `rust-toolchain.toml` and pins dependency
resolution in `Cargo.lock`. Rustup honors the toolchain file automatically; use
`--locked` for dependency-resolving build, test, check, and clippy commands. No
MySQL is needed for development when the in-memory backend is selected
explicitly.

```bash
cd /web/deltamud/rust-mud
cargo build --release --locked

# Development (mock DB, no MySQL):
MUD_MOCK_DB=true MUD_BIND=127.0.0.1 MUD_PORT=4000 \
  MUD_LIB_PATH=/web/deltamud/lib ./target/release/deltamud

# Initialize or upgrade a real database, then exit. Run this before normal boot:
MUD_MOCK_DB=false DATABASE_URL="mysql://deltamud:<pw>@127.0.0.1/deltamud" \
  ./target/release/deltamud --migrate

# Local production-configuration smoke only. Real production is launched from
# an immutable installed release by the trusted manager in docs/RUNBOOK.md.
MUD_MOCK_DB=false \
DATABASE_URL="mysql://deltamud:<pw>@127.0.0.1/deltamud" \
MUD_BIND=127.0.0.1 \
MUD_LIB_PATH=/var/lib/deltamud/lib \
MUD_METRICS_PORT=19595 \
./target/release/deltamud
```

Connect with any Telnet/MUD client: `telnet <host> 4000` (or `nc` for scripts).
The first character is an ordinary level-1 mortal. For a local/development durable database,
create that character normally, stop the server, and perform the one-time
offline promotion:

```bash
MUD_MOCK_DB=false DATABASE_URL="mysql://deltamud:<pw>@127.0.0.1/deltamud" \
  ./target/release/deltamud --bootstrap-implementor Founder
```

The target must already be a durable player. The command exits without starting
the listener and refuses to run if an effective Implementor already exists or a
live Rust server/other maintenance command owns the same database. Normal
MySQL-backed servers hold and continuously verify that database-scoped
runtime/maintenance lease for their process lifetime; loss of ownership stops
the server fail-closed. The in-memory mock backend is exempt, and maintenance
modes reject mock configuration.

Production uses the root-owned trusted manager's backup-backed
`bootstrap-implementor <sha> <name> --acknowledge-offline-authority-bootstrap`
workflow; never run a checkout binary against production. See
[`docs/RUNBOOK.md`](docs/RUNBOOK.md).

### Configuration (environment variables)
| Var | Default | Notes |
|---|---|---|
| `MUD_BIND` | `0.0.0.0` | Game-listener IPv4/IPv6 address. The systemd example binds `127.0.0.1` for a reviewed edge proxy. |
| `MUD_PORT` | `4000` | Game listen port. |
| `MUD_LIB_PATH` | `./lib` | World/data and runtime-state dir. Development can use `/web/deltamud/lib` only when no other server writes it; production should use a private copy such as `/var/lib/deltamud/lib`. |
| `MUD_MOCK_DB` | build-dependent | Debug/test defaults to mock; release defaults to real. Set it explicitly: `true` is ephemeral and `false` is MySQL. Invalid values fail startup. |
| `DATABASE_URL` | *(none)* | Required and non-empty whenever the real backend is selected. There is no compiled-in credential or automatic fallback. Normal startup verifies the schema but never migrates it. |
| `MUD_DB_TIMEOUT_SECS` | `5` | Application-boundary timeout for each database operation. |
| `MUD_METRICS_PORT` | *(off)* | Enables `/metrics`, `/live`, `/ready`, `/health`, and `/api/who`. Invalid values and listener bind failures abort startup. **Avoid 9200/9201** — Elasticsearch on this host owns them; use e.g. `19595`. |
| `MUD_METRICS_BIND` | `127.0.0.1` | Metrics bind IP; invalid values abort startup. Keep loopback unless access is restricted by a firewall or reverse proxy. |
| `MUD_EXEC_PATH` | current executable | Optional copyover target. Production requires an absolute, executable regular file whose binary and ancestor directories are root-owned and not group/world-writable; use `/opt/deltamud/current/bin/deltamud`. |
| `MUD_RNG_SEED` | *(clock)* | Pins the Lehmer PRNG for reproducible/golden runs. |
| `MUD_NO_SPECIALS` / `-s` | off | Skip special-procedure assignment (C's `-s` flag). `-q` is not treated as no-specials. |
| `MUD_MAX_CONN` | `256` | Concurrent-connection cap; `MUD_CONN_BURST`/`MUD_CONN_WINDOW_MS` add per-IP rate limiting. |
| `MUD_REVERSE_DNS` | `true` | Resolve peer PTR names at the socket edge; only forward-confirmed names are trusted. Set `false`/`0` to use canonical IPs only. |
| `MUD_REVERSE_DNS_TIMEOUT_MS` | `1000` | Whole PTR + forward-confirmation deadline per connection (clamped to 1–10000 ms); timeout falls back to the canonical peer IP. |
| `MUD_REVERSE_DNS_MAX_INFLIGHT` | `16` | Maximum simultaneous blocking system-resolver calls (clamped to 1–256). |
| `RUST_LOG` | `info` | Log level. |

### Control / ops
- **Copyover** (`copyover` command, immortal): validates `MUD_EXEC_PATH`, durably saves players and all outstanding OLC work, publishes a checksummed recovery snapshot, then re-execs while keeping playing connections attached. Any OLC durability failure aborts the exec and leaves the dirty entries pending.
- **Administrative authority**: staff dispatch and sensitive disclosures resolve the exact authenticated player principal, persisted trust, quarantine state, and GCMD grants. Forced/scripted commands cannot spend staff authority. `advance` is the durable level/trust/grant path; `set level`, `set trust`, and `set cmd*` are rejected for players.
- **Deferred destructive commands**: copyover, shutdown/reboot, and pfileclean retain the initiating descriptor/principal tuple and revalidate it immediately before the delayed effect. Disconnect, demotion, quarantine, or grant revocation cancels the request.
- **Graceful process control**: `SIGTERM`, `Ctrl-C`, and `shutdown die/pause`
  save all players and exit 0 so systemd leaves the service stopped. Scheduled
  reboot and `shutdown`/`shutdown reboot`/`shutdown now` save cleanly and exit
  75, which the supplied unit explicitly restarts.
- **Deployment scaffold**: `deploy/systemd/` provides hardened runtime and offline-migration units, protected environment/backup templates, and tmpfiles rules. Install the independently reviewed `scripts/release.sh` as the root-owned trusted manager; never run a checkout-controlled copy with privilege. It builds exact revisions with `--locked` under a dedicated unprivileged account, validates immutable releases and `/ready`, and preserves state-changing failures for operator reconciliation. See [`docs/RUNBOOK.md`](docs/RUNBOOK.md).
- **CI**: repository-root `.github/workflows/rust-mud-ci.yml` runs formatting, build, tests, clippy, MariaDB persistence, and bounded live canaries on push.

## Testing

```bash
cargo test --locked                         # complete suite
cargo test --locked <substring>             # a single test by substring
scripts/clippy-check.sh                      # -D warnings plus explicit legacy lint baseline
PYTHONDONTWRITEBYTECODE=1 python3 scripts/playthrough_test.py -v
```
The tracked `scripts/canary.sh` runner creates isolated state and enforces
bounded semantic health/combat checks; `scripts/parity-check.sh` compares the C
and Rust servers with acknowledged command transcripts. See
[`docs/RUNBOOK.md`](docs/RUNBOOK.md) for smoke and longer-cadence commands.
World source files are broadly compatible with the C MUD, while runtime-file
format selection and migration caveats are recorded in
[`COMPATIBILITY.md`](COMPATIBILITY.md). Use the C build
(`/web/deltamud/bin/circle`) side-by-side as the comparison oracle when proving
exact parity.

## Architecture (in brief)

- `state.rs` — `GameState`, the single owner of the world (id-indexed `chars`/`objs` `IndexMap`s, `rooms` `Vec`, prototype tables, descriptors).
- `game.rs` — the one async Game task: drains player input, runs the 10 Hz heartbeat, flushes output. Commands run synchronously here against `&mut GameState`.
- `interpreter.rs` + `command_table.rs` — the `CMD_INFO` table (1:1 with C `cmd_info[]`, order is load-bearing) dispatched by `HandlerId`.
- `connection.rs` — per-socket async I/O + the telnet/IAC/GMCP layer; commands never touch sockets (they append to `Descriptor.outbuf`).
- `act.rs` — the `$n/$N/...` message engine. `dg_*.rs` — the script engine. `*edit.rs`/`olc.rs` — the builder suite. `database*.rs`/`objsave.rs`/`password.rs` — persistence.

See `CLAUDE.md` for the full picture (output flow, heartbeat cadence, the surface-map room splice, copyover internals, the borrow-discipline "house style").

## License & credits

A derivative work of CircleMUD 3.0; the original CircleMUD license applies. Thanks to the original DeltaMUD team, the CircleMUD creators, and the Rust ecosystem.
