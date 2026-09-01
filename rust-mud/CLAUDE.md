# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

`rust-mud/` is a from-scratch Rust reimplementation of DeltaMUD (a CircleMUD 3.0 derivative). It is a broad, near-complete Rust port with a layer of modern improvements. As of the 2026-08 parity program (GitHub issues #96-#347, epic #348) all confirmed fidelity gaps from the 12-subsystem audit are closed; the remaining deliberate divergences (C-bug repairs) are registered in `COMPATIBILITY.md`.

For high-level status, read [`README.md`](README.md). For operational compatibility caveats, read [`COMPATIBILITY.md`](COMPATIBILITY.md). This file is the detailed agent/developer guide.

## The C source is the oracle

`/web/deltamud/src/*.c` (and `*.h`) is the **read-only authoritative reference**. The Rust port aims to match its output strings, world-file grammar, numeric formulas, and DB schema, with the current exceptions listed below. When porting or fixing behavior, read the corresponding C function first and match it; never invent behavior not in the C. The C MUD still builds and boots (`/web/deltamud/bin/circle`) as a live comparison oracle.

## Current parity snapshot

The command surface is substantially ported: the Rust `CMD_INFO` table matches the C command table order, and real command handlers are wired rather than falling through to the generic unimplemented path. The major subsystems also exist: 83-column SQL player persistence, world loaders, DG VM and command sets, OLC editors, copyover, telnet/IAC filtering, GMCP/MSSP, shops, clans, boards, mail, houses, quest, auction, arena, combat, magic, and special procedures.

Remaining compatibility considerations are mostly coexistence and migration
policy rather than known parity gaps:

- **Persistence compatibility:** SQL `player_main` is broad and current, and player aliases round-trip through C-compatible `plralias/<bucket>/<name>.alias` sidecars. Rent/crash objects, houses, boards, clans, and mail auto-detect supported C/Rust representations and preserve the detected format on atomic rewrite; legacy Rust representations remain readable. `MUD_CFORMAT_FILES` selects C format only for new or intrinsically ambiguous empty stores. Never let two servers write the same live `lib` tree.
- **OLC/editor fidelity:** the main OLC save dispatcher covers room, object, mobile, zone, and shop writers; redit/oedit/medit expose DG attachment editing; and inline OLC text buffers share the C-style `modify.rs` string-editor command set (`/a /c /d /e /f /fi /i /h /l /n /r /ra /s`). The durability model is described below and in `docs/RUNBOOK.md`.

Recently resolved tracker-backed parity fixes:

- Complex aliases now expand through the descriptor input queue one command per pulse, preserving C-style wait timing (#86).
- Character creation now walks the C nanny sequence for newbie, sex, race, deity, class, hometown/stat rolls, and `do_start_init` setup (#87).
- Central `olc save` dispatches room/object/mobile/zone/shop disk writers (#88).
- DG attachment editing is wired in redit/oedit/medit, and room saves preserve room `T` trigger attachment lines (#89).
- Social minimum levels, `wizhelp` GCMD filtering, and the static `insult` social listing are covered (#90).
- Hostname-based `BAN_NEW` and `BAN_SELECT` login gates are implemented (#91).
- Immortal raw-kill and `deathblow` now use dedicated side-effect paths instead of normal damage routing (#92).
- Runtime fidelity fixes now cover descriptor-host SQL saves, played-time save accounting, corrupted gold/bank clamps, `slist` spell rows, broader `APPLY_*` handling, SQL-backed autowiz enumeration, SQL-backed clan roster display, and MOB_CASTER magic-user binding.
- Inline OLC text buffers in redit/oedit/medit/hedit/trigedit now share the generic C-style string-editor parser, including `/fi` and `/ra`.

## Build / run / test

The repository pins Rust 1.98.0 through `rust-toolchain.toml` and dependency
resolution through `Cargo.lock`. Keep both files under source control. Rustup
selects the pinned toolchain automatically; every command that resolves
dependencies must use `--locked`.

```bash
cd /web/deltamud/rust-mud
cargo build --locked
cargo build --release --locked       # thin LTO; symbols retained; panic=unwind
cargo test --locked                  # complete suite
cargo test --locked <name>           # a single test by substring
scripts/clippy-check.sh                 # -D warnings + explicit legacy lint baseline
cargo fmt --all -- --check

# Run explicitly with the ephemeral development database:
MUD_MOCK_DB=true MUD_BIND=127.0.0.1 MUD_PORT=4000 \
  MUD_LIB_PATH=/web/deltamud/lib ./target/release/deltamud
```

Key env vars (read in `config.rs` + `main.rs`):
- `MUD_MOCK_DB=true` selects the in-memory `MockDatabase` (ephemeral across a cold restart); `false` selects MySQL. Debug/test builds default to mock and release builds default to real, but commands and deployments must set the mode explicitly. Invalid boolean values fail configuration.
- `DATABASE_URL` has no default and is required, non-empty, whenever the real backend is selected. Normal startup verifies the checksummed schema and fails closed if it is missing or stale; only `deltamud --migrate` applies migrations.
- `MUD_BIND` is the game-listener IPv4/IPv6 address (default `0.0.0.0`); `MUD_PORT` defaults to 4000. The systemd scaffold intentionally uses `127.0.0.1` behind a separately reviewed edge proxy.
- `MUD_LIB_PATH` is the world/data and writable runtime-state dir. A development process may use `/web/deltamud/lib` only when no C/Rust peer is writing it; production uses a private copy such as `/var/lib/deltamud/lib`.
- `MUD_RNG_SEED=<n>` pins the Lehmer PRNG for golden tests (same seed => identical zone prime / combat); `MUD_NO_SPECIALS` or argv `-s` enables C-compatible no-specials mode. `-q` is not treated as no-specials.
- `MUD_METRICS_PORT=<port>` enables `/metrics`, `/live`, `/ready`, `/health`, and `/api/who`. `/live` proves only that the HTTP task responds; `/ready` also requires completed boot and a heartbeat no more than two seconds old. Invalid metrics addresses/ports and bind failures abort startup. **Never use 9200/9201 — this box's Elasticsearch owns them; use e.g. 19595.**
- `MUD_EXEC_PATH` optionally selects the copyover binary. A configured value must be absolute and resolve at copyover time to an executable regular file; production additionally requires the binary and both path chains to be root-owned and not group/world-writable. Production uses `/opt/deltamud/current/bin/deltamud`; development falls back to `current_exe()`.
- `MUD_MAX_CONN` (default 256), `MUD_CONN_BURST`/`MUD_CONN_WINDOW_MS` (per-IP rate limit).
- `MUD_REVERSE_DNS` (default true), `MUD_REVERSE_DNS_TIMEOUT_MS` (default 1000), and `MUD_REVERSE_DNS_MAX_INFLIGHT` (default 16) enable bounded FCrDNS host identity. Ban checks always include the canonical socket peer IP; lookup failure/timeout falls back to that IP.

Real-database setup is deliberately offline and explicit:

```bash
MUD_MOCK_DB=false DATABASE_URL="mysql://deltamud:<pw>@127.0.0.1/deltamud" \
  ./target/release/deltamud --migrate
```

The first created character is an ordinary level-1 mortal. On a new durable
database, create the intended administrator normally, stop the server, then run
the one-time offline promotion below. It refuses a nonexistent/non-player
target, a mock database, or any database that already contains an Implementor.

```bash
MUD_MOCK_DB=false DATABASE_URL="mysql://deltamud:<pw>@127.0.0.1/deltamud" \
  ./target/release/deltamud --bootstrap-implementor Founder
```

### Testing against a running server
Scripted Telnet with raw `nc` (it does not answer the server's initial `WILL GMCP`, so GMCP remains disabled; expect raw IAC bytes at the start of captured output):
```bash
( printf 'Tester\r\ny\r\npass\r\npass\r\ny\r\nm\r\na\r\na\r\nc\r\ny\r\n'; sleep 1; printf 'score\r\n'; sleep 0.5; printf 'quit\r\n' ) | nc -q9 127.0.0.1 4000
```
Gotchas that waste time:
- **No character is implicitly privileged.** Even `idnum == 1` starts as a mortal; use the one-time offline bootstrap above for the initial Implementor, then authenticated in-game administration.
- `valid_name` is **alpha-only** — names with digits ("Test2") are rejected at the name prompt.
- Stop a development server by the exact PID captured when it was launched
  (`kill -TERM "$server_pid"; wait "$server_pid"`). Never use `pkill`: this host
  may have another DeltaMUD process whose lifecycle is outside your test.
- Use `scripts/canary.sh` for the isolated concurrent live workload; it owns a
  private world copy, ports, exact server PID, cleanup deadline, and evidence
  directory.
- `cargo clean` wipes `target/`, so rebuild before running.

## Architecture — the big picture

**Single-owner `GameState`, async only at the edge.** This is the defining decision (the explicit goal was "use Rust, not C-in-Rust"): there is **no `Arc<RwLock>` entity graph**. `state::GameState` owns the entire world in id-indexed arenas:
- `chars: IndexMap<CharId, Character>`, `objs: IndexMap<ObjId, Object>` (IndexMap gives O(1) insert + O(1) `swap_remove` + ordered iteration; entities reference each other by `Copy` ids — `CharId`/`ObjId`/`RoomRnum`/`ConnId` — never by pointer). `rooms: Vec<Room>` indexed by `RoomRnum` with a `room_index` (vnum→rnum) map.
- `descriptors: HashMap<ConnId, Descriptor>`, `players_by_name`, `player_table` (the offline name↔idnum index), prototype tables (`mob_protos`, `obj_protos`, `zones`), and the `rng`.

**Commands are synchronous** `fn(&mut GameState, ch: CharId, arg: &str, subcmd: i32)`. They mutate the world directly with no locks. Tokio lives only in `connection.rs` (per-socket read/write tasks) and `game.rs` (the one Game task). This mirrors CircleMUD's actually-single-threaded heartbeat and is deadlock-free.

**The Game task is the whole game loop** (`game.rs::run`): a `tokio::select!` over the input channel (`GameMessage`), a 100 ms / 10 Hz heartbeat, and shutdown signals. Because every command + heartbeat handler runs in this one task, **a panic there would kill the whole server** — so dispatch and the heartbeat are wrapped in `catch_unwind` (`dispatch_command_isolated` / `heartbeat` → `heartbeat_inner`), and a panic hook logs a backtrace. **`[profile.release]` must keep `panic = "unwind"`** or `catch_unwind` stops working.

**Output is buffered, not direct.** Handlers call `state.send_to_char` / `act()` which append to `Descriptor.outbuf` (a `String`). The Game task's `flush_all` renders color (`render_color`, single-pass `&x`→ANSI) and pushes to each connection's `mpsc::Sender<String>`; the per-conn writer task does the socket write. Raw control bytes (telnet IAC, GMCP/MSSP subnegotiation) bypass `outbuf` via `send_raw_bytes` (they'd be mangled by color rendering).

**Command dispatch** (`interpreter.rs` + `command_table.rs`): `CMD_INFO` is a 1:1 transcription of C's `cmd_info[]` (table order is load-bearing — it's the abbreviation-priority + `sprintbit`/`sprinttype` index contract). Each entry has a `HandlerId` enum variant; `command_interpreter` does prefix-match + level/position/`godcmd` gating, then `dispatch()` routes the `HandlerId` to the handler. Socials are spliced in as a fallback. `WAIT_STATE` command lag is real: input is queued per descriptor and drained one command per pulse through `Descriptor.wait`.

**Heartbeat cadence** (`game.rs::heartbeat_inner`, pulses at 10 Hz): `perform_violence` (PULSE_VIOLENCE), `mobile_activity` (PULSE_MOBILE), `zone_update`, `dg_event::process_events` (every pulse), `script_trigger_check` (130), `point_update`+weather (750 = one MUD hour), `weather_activity` (300), `quest_update`+`blood_update` (600), `crash_save_all` (750).

**Subsystem map** (find by name; each port is intended to track the C oracle, with current gaps listed above):
- `act()` message engine (`act.rs`, from comm.c `perform_act`): `$n/$N/$m/$s/$e/$o/$p` substitution + per-recipient visibility.
- DG Scripts VM: `dg_scripts.rs` (`script_driver`, depth guard 10 + while-loop guard 30), `dg_handler.rs`/`dg_event.rs`/`dg_db_scripts.rs`, `dg_{mob,obj,wld}cmd.rs`, fire-hooks in `dg_triggers.rs`. **Triggers must boot before the world** (`main.rs` calls `boot_dg_scripts` before `load_world`).
- OLC: `olc.rs` + `redit/oedit/medit/zedit/sedit/aedit/hedit/trigedit.rs` (nested-input editors; per-conn state in module-static `OnceLock<Mutex<...>>` keyed by `ConnId`; room/object/mobile/zone/shop central save dispatch is wired; DG attachment editors are exposed in redit/oedit/medit; inline text buffers share the C-style `modify.rs` string-editor command parser). File publication is durable: unique sibling temp, checked write/flush/fsync, atomic rename, then parent-directory fsync. Disk-first editors publish live state only after replacement succeeds. REDIT/OEDIT retain the C two-stage model (commit to memory and mark the save list, then `olc save` writes the zone); central writers remove a save-list item only after durable replacement. A failed OLC flush blocks manual success, shutdown, auto-reboot, and copyover while retaining dirty entries.
- Persistence: `database.rs` (real 83-column `player_main` + `player_affects`/`player_skills` and ordered checksummed migrations), `database_compat.rs` (the column<->Character mapping), `mock_database.rs`, `objsave.rs`, and `password.rs`. New passwords are Argon2id PHC strings using RustCrypto defaults and OS randomness; successful verification of supported DES, SHA-crypt, bare SHA-256, or weak Argon2id records attempts a credential-column compare-and-swap upgrade. Password verification rejects inputs above 64 bytes before invoking a KDF.
- Spec procs (`spec_procs.rs`/`spec_assign.rs`), combat (`combat.rs`: DeltaMUD's `chance()`/`dam_multi()` from utils.c, not stock THAC0), magic (`magic.rs`/`spell_parser.rs`/`spells.rs`), economy (`shop/clan/boards/mail/house/quest/auction`).

## Things that are NOT obvious

- **Surface map = ~9,801 synthetic rooms** spliced into `GameState.rooms` *after* the 600 real rooms (`maputils.rs::integrate_map_rooms`; vnums 2,000,000+, rnums ≥ `map_start_rnum`). Real-room rnums are untouched. Any "iterate all rooms" loop in a hot path must stop at `map_start_rnum` (see `script_trigger_check`).
- **Copyover is real seamless reattach** (`do_copyover` in `cmd_wizard.rs` + recovery in `main.rs`): it resolves and validates `MUD_EXEC_PATH` (or the current executable in development), durably publishes a versioned/count-checked/SHA-256-checked snapshot, clears FD_CLOEXEC only after preconditions pass, and `execv`s with `--copyover`. Recovery validates the complete snapshot and inherited fds before adopting any socket; missing durable MySQL rows fail recovery instead of being silently recreated.
- **Telnet/GMCP/MSSP layer** lives in `connection.rs` (`TelnetFilter`) + `game.rs`: fresh connections receive server-initiated `WILL GMCP`; recovered connections reset and re-offer it. Negotiated descriptors retain bounded `Core.Hello` and `Core.Supports.Set/Add/Remove` state and receive `Char.Vitals`/`Room.Info`; unsupported clients receive no GMCP payload. MSSP and plain Telnet remain compatible. UTF-8 negotiation, NAWS, TTYPE/MTTS, and MCCP are intentionally deferred.
- **House style** (stated in `cmd_informative.rs`, followed throughout): copy scalars / clone collections into locals *before* any `send`/`act`, re-look-up entities by id, and never hold a borrow across a mutation — this keeps the borrow checker happy given `&mut GameState` everywhere.
- Offline-player immortal commands (`set`/`stat` a logged-off player) go through an **async bridge** (`GameState.offline_ops` → `game.rs::drain_offline_ops`): load → instantiate → replay the command → save → extract.

## Git

Repo is `/web/deltamud` (its own git repo, `github.com/faratech/deltamud`, branch `main`, pushes straight to origin). Stage explicit paths (`git add rust-mud/src`), not `git add -A`; preserve unrelated runtime artifacts such as `lib/plrobjs/` and `lib/etc/clans.dat`. `rust-mud/Cargo.lock` and `rust-mud/rust-toolchain.toml` are release inputs and should be committed. Commit messages end with the `Co-Authored-By:` trailer.
