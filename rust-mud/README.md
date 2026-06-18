# DeltaMUD — Rust Edition

A broad Rust reimplementation of DeltaMUD (a CircleMUD 3.0 derivative), plus a layer of modern improvements. The C source at `/web/deltamud/src` is the authoritative reference; this port is intended to match its output strings, world-file formats, numeric formulas, and database schema while replacing the C pointer graph with an idiomatic single-owner design. It is not exact C feature parity yet; see `COMPATIBILITY.md` for the current gap list.

~83 modules / ~75k lines. Builds clean and boots against the original `lib/` world files.

> For development guidance (architecture deep-dive, conventions, gotchas), see **`CLAUDE.md`** in this directory.

## What's implemented

The Rust port has the broad DeltaMUD feature surface in place, including the static command table and major subsystems:

- **World**: rooms/objects/mobiles/zones/shops loaders (incl. `E` extra-descriptions, `A` applies, room `O` special exits, mob `X`-stats + espec, DG `T` triggers), zone resets with load-chance gating.
- **Gameplay**: combat (DeltaMUD `chance()`/`dam_multi()` from `utils.c` — not stock THAC0 — plus avoid/parry/dodge/riposte), magic/skills (spell_parser/magic/spells), classes/races/deities/languages, regen, affects, conditions, `WAIT_STATE` command lag.
- **Content & economy**: shops, clans, boards, mail, houses, quests, auction, arena, the castle/special procedures.
- **DG Scripts**: full VM (`script_driver` with depth + loop guards), mob/obj/wld trigger command sets, and fire-hooks (greet/command/speech/death/load/timer/random).
- **OLC**: redit / oedit / medit / zedit / sedit / aedit / hedit / trigedit, with many save-to-disk paths implemented and remaining caveats tracked in `COMPATIBILITY.md`.
- **Persistence**: 83-column `player_main` + `player_affects` + `player_skills` (MySQL), Rust-format object rent/crash files, crypt-compatible passwords; offline-player immortal ops via an async bridge.
- **Immortal tooling**: the full `act.wizard` command set, god-command (GCMD) permission bits, `can_edit_zone`, autowiz, on-disk syslog, the player-index table.

Known remaining C-parity gaps are tracked in `COMPATIBILITY.md`. The highest-risk areas are C runtime persistence-file compatibility, shared string-editor flows, character creation, combat edge cases, alias persistence/timing, OLC/DG attachment saves, and some admin policy details.

### Modern improvements over the C version
- **Idiomatic Rust core**: a single-owner `GameState` (id-indexed `IndexMap`/`Vec` arenas, `Copy` ids instead of locked pointers) — deadlock-free, with async (Tokio) only at the socket edge. No `Arc<RwLock>` entity graph.
- **Seamless copyover**: hot-reboots the binary (`execv` + inherited socket fds) while keeping every connection attached — same as the C MUD.
- **Crash isolation**: a panic in any command or heartbeat handler is contained (`catch_unwind`) and logged with a backtrace, instead of killing the server.
- **Modern client protocols**: telnet IAC negotiation, password echo suppression, **GMCP** (`Char.Vitals`/`Room.Info` — Mudlet gauges + auto-map) and **MSSP** (server status for MUD listings).
- **Observability**: optional Prometheus `/metrics` + `/health` endpoint (heartbeat tick-timing, player/mob/obj gauges, command/connection counters), syslog rotation.
- **Hardening**: IP ban at accept, connection rate-limit + max-connections, graceful SIGTERM/Ctrl-C shutdown with save-all.

## Building & running

Requires a recent stable Rust toolchain. No MySQL needed for development (an in-memory mock DB round-trips full player state within a run).

```bash
cd /web/deltamud/rust-mud
cargo build --release

# Development (mock DB, no MySQL):
MUD_MOCK_DB=true MUD_PORT=4000 MUD_LIB_PATH=/web/deltamud/lib ./target/release/deltamud

# With the metrics/health endpoint and production MySQL:
DATABASE_URL="mysql://root:<pw>@127.0.0.1/deltamud" \
MUD_LIB_PATH=/web/deltamud/lib \
MUD_METRICS_PORT=19595 \
./target/release/deltamud
```

Connect with any telnet/MUD client: `telnet <host> 4000` (or `nc` for scripts). The **first character created becomes the Implementor** (idnum 1, level 105).

### Configuration (environment variables)
| Var | Default | Notes |
|---|---|---|
| `MUD_PORT` | `4000` | Game listen port. |
| `MUD_LIB_PATH` | `./lib` | World/data dir — use `/web/deltamud/lib`. |
| `MUD_MOCK_DB` | `false` | `true` = in-memory DB (dev). Unset/`false` → real MySQL. |
| `DATABASE_URL` | `mysql://root:password@localhost/deltamud` | Used when not mocking. Tables auto-create. |
| `MUD_METRICS_PORT` | *(off)* | Enables `/metrics` + `/health`. **Avoid 9200/9201** — Elasticsearch on this host owns them; use e.g. `19595`. |
| `MUD_RNG_SEED` | *(clock)* | Pins the Lehmer PRNG for reproducible/golden runs. |
| `MUD_NO_SPECIALS` / `-s` | off | Skip special-procedure assignment (C's `-s` flag). Rust currently also treats `-q` this way, which is a known C-parity bug. |
| `MUD_MAX_CONN` | `256` | Concurrent-connection cap; `MUD_CONN_BURST`/`MUD_CONN_WINDOW_MS` add per-IP rate limiting. |
| `RUST_LOG` | `info` | Log level. |

### Control / ops
- **Copyover** (`copyover` command, immortal): re-execs the binary keeping players connected.
- **Graceful shutdown**: `SIGTERM` / `Ctrl-C` saves all players and exits cleanly.
- **CI**: `.github/workflows/ci.yml` runs `cargo build --release`, `cargo test`, `cargo clippy` on push.

## Testing

```bash
cargo test                 # unit tests (DG-script parsing, password vectors, ...)
cargo test <substring>     # a single test
```
A 3-player concurrent soak script lives at `/tmp/soak.py <port>` (expects 0 panics). World source files are broadly compatible with the C MUD, but some Rust OLC save paths still have known gaps; use the C build (`/web/deltamud/bin/circle`) side-by-side as the comparison oracle when proving exact parity.

## Architecture (in brief)

- `state.rs` — `GameState`, the single owner of the world (id-indexed `chars`/`objs` `IndexMap`s, `rooms` `Vec`, prototype tables, descriptors).
- `game.rs` — the one async Game task: drains player input, runs the 10 Hz heartbeat, flushes output. Commands run synchronously here against `&mut GameState`.
- `interpreter.rs` + `command_table.rs` — the `CMD_INFO` table (1:1 with C `cmd_info[]`, order is load-bearing) dispatched by `HandlerId`.
- `connection.rs` — per-socket async I/O + the telnet/IAC/GMCP layer; commands never touch sockets (they append to `Descriptor.outbuf`).
- `act.rs` — the `$n/$N/...` message engine. `dg_*.rs` — the script engine. `*edit.rs`/`olc.rs` — the builder suite. `database*.rs`/`objsave.rs`/`password.rs` — persistence.

See `CLAUDE.md` for the full picture (output flow, heartbeat cadence, the surface-map room splice, copyover internals, the borrow-discipline "house style").

## License & credits

A derivative work of CircleMUD 3.0; the original CircleMUD license applies. Thanks to the original DeltaMUD team, the CircleMUD creators, and the Rust ecosystem.
