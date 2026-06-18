# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

`rust-mud/` is a from-scratch Rust reimplementation of DeltaMUD (a CircleMUD 3.0 derivative). It is a broad, near-complete Rust port with a layer of modern improvements, but it is **not exact C feature parity yet**. ~83 modules / ~75k lines.

For high-level status, read `README.md`. For operational compatibility caveats, read `COMPATIBILITY.md`. This file is the detailed agent/developer guide.

## The C source is the oracle

`/web/deltamud/src/*.c` (and `*.h`) is the **read-only authoritative reference**. The Rust port aims to match its output strings, world-file grammar, numeric formulas, and DB schema, with the current exceptions listed below. When porting or fixing behavior, read the corresponding C function first and match it; never invent behavior not in the C. The C MUD still builds and boots (`/web/deltamud/bin/circle`) as a live comparison oracle.

## Current parity snapshot

The command surface is substantially ported: the Rust `CMD_INFO` table matches the C command table order, and real command handlers are wired rather than falling through to the generic unimplemented path. The major subsystems also exist: 83-column SQL player persistence, world loaders, DG VM and command sets, OLC editors, copyover, telnet/IAC filtering, GMCP/MSSP, shops, clans, boards, mail, houses, quest, auction, arena, combat, magic, and special procedures.

Known remaining parity work is mostly integration and fidelity detail, not whole missing systems:

- **Persistence compatibility:** SQL `player_main` is broad and current, and player aliases now round-trip through C-compatible `plralias/<bucket>/<name>.alias` sidecars. Rent/crash object files, houses, boards, clans, and mail still use Rust text formats rather than C raw on-disk records. Do not share live C persistence files with Rust without a migration/compatibility pass.
- **Character creation:** the Rust nanny path skips C states for newbie prompts, deity, hometown, stat reroll/accept, and creation-time `do_start_init` (#87).
- **Combat and flags:** `deathblow` and immortal raw-kill semantics still route through normal damage; the current tracker item is #92.
- **Command semantics:** complex aliases execute immediately instead of being queued through descriptor wait (#86), social minimum level is not enforced, `wizhelp` ignores per-command GCMD bits, and `socials` omits the static `insult` social (#90).
- **OLC/DG authoring:** trigger prototype editing is strong, but redit/oedit/medit do not expose the C DG attachment-list editor, room saves drop room `T` trigger attachments (#89), and central `olc save` does not dispatch every editor's disk writer (#88).
- **Runtime/admin policy:** ban enforcement misses C hostname, `BAN_NEW`, and `BAN_SELECT` paths (#91).
- **Other fidelity gaps:** `slist` emits no spell rows, several `APPLY_*` locations are narrower than C, autowiz/clan offline reporting are not fully SQL-backed, and some OLC text editors do not support the full C `/a /c /d /e /f /i /h /l /n /r /s` command set.

## Build / run / test

```bash
cd /web/deltamud/rust-mud
cargo build                 # debug
cargo build --release       # optimized (LTO+strip; ~1.5 min from clean)
cargo test                  # ~27 tests (mostly DG-script parsing, in dg_*.rs + password.rs)
cargo test <name>           # a single test by substring
cargo clippy --all-targets  # lint (CI runs this; not gated)

# Run (default = in-memory mock DB; no MySQL needed):
MUD_MOCK_DB=true MUD_PORT=4000 MUD_LIB_PATH=/web/deltamud/lib ./target/release/deltamud
```

Key env vars (read in `config.rs` + `main.rs`):
- `MUD_MOCK_DB=true` — use the in-memory `MockDatabase` (default for dev; round-trips full player state within one run). Unset / `false` → real MySQL via `DATABASE_URL` (default `mysql://root:password@localhost/deltamud`).
- `MUD_LIB_PATH` — the world/data dir (use `/web/deltamud/lib`, the shared C lib).
- `MUD_PORT` (default 4000), `MUD_RNG_SEED=<n>` (pins the Lehmer PRNG for golden tests — same seed => identical zone prime / combat), `MUD_NO_SPECIALS` (or argv `-s`; C-compatible no-specials mode. `-q` is not treated as no-specials).
- `MUD_METRICS_PORT=<port>` — enables a Prometheus `/metrics` + `/health` HTTP endpoint. **Never use 9200/9201 — this box's Elasticsearch owns them; use e.g. 19595.**
- `MUD_MAX_CONN` (default 256), `MUD_CONN_BURST`/`MUD_CONN_WINDOW_MS` (per-IP rate limit).

### Testing against a running server
Scripted-telnet test with raw `nc` (the IAC input filter now tolerates telnet negotiation, but `nc` avoids it entirely):
```bash
( printf 'Tester\r\ny\r\npass\r\npass\r\nm\r\nw\r\nh\r\n'; sleep 1; printf 'score\r\n'; sleep 0.5; printf 'quit\r\n' ) | nc -q9 127.0.0.1 4000
```
Gotchas that waste time:
- **First character created becomes the Implementor** (`idnum == 1`, level 105, all god-command bits). Later characters are mortals — many immortal commands (`goto`, `load`, `stat`, `set`, `skillset`) will return "Huh?!?" for them.
- `valid_name` is **alpha-only** — names with digits ("Test2") are rejected at the name prompt.
- Kill the server with `pkill -x deltamud` (not `pkill -f` — that can match your own shell). `cargo clean` wipes `target/`, so rebuild before running.
- `/tmp/soak.py <port>` is the 3-player concurrent soak (expects 0 panics).

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
- OLC: `olc.rs` + `redit/oedit/medit/zedit/sedit/aedit/hedit/trigedit.rs` (nested-input editors; per-conn state in module-static `OnceLock<Mutex<...>>` keyed by `ConnId`; explicit save paths and DG attachment editors still need parity work).
- Persistence: `database.rs` (real 83-column `player_main` + `player_affects`/`player_skills`), `database_compat.rs` (the column<->Character mapping), `mock_database.rs`, `objsave.rs` (Rust-format rent/crash object files, not C binary compatible), `password.rs` (crypt-compatible).
- Spec procs (`spec_procs.rs`/`spec_assign.rs`), combat (`combat.rs`: DeltaMUD's `chance()`/`dam_multi()` from utils.c, not stock THAC0), magic (`magic.rs`/`spell_parser.rs`/`spells.rs`), economy (`shop/clan/boards/mail/house/quest/auction`).

## Things that are NOT obvious

- **Surface map = ~9,801 synthetic rooms** spliced into `GameState.rooms` *after* the 600 real rooms (`maputils.rs::integrate_map_rooms`; vnums 2,000,000+, rnums ≥ `map_start_rnum`). Real-room rnums are untouched. Any "iterate all rooms" loop in a hot path must stop at `map_start_rnum` (see `script_trigger_check`).
- **Copyover is real seamless reattach** (`do_copyover` in `cmd_wizard.rs` + recovery in `main.rs`): it `execv`s the binary with `--copyover`, inheriting the socket fds (FD_CLOEXEC cleared first; `execv` skips Rust drops so the fds survive). Connections stay attached across the reboot. Uses `libc`.
- **Telnet/GMCP/MSSP layer** lives in `connection.rs` (`TelnetFilter`) + `game.rs`: IAC negotiation is stripped from input, passwords use `IAC WILL/WONT ECHO`, and GMCP `Char.Vitals`/`Room.Info` + MSSP status are pushed for modern clients.
- **House style** (stated in `cmd_informative.rs`, followed throughout): copy scalars / clone collections into locals *before* any `send`/`act`, re-look-up entities by id, and never hold a borrow across a mutation — this keeps the borrow checker happy given `&mut GameState` everywhere.
- Offline-player immortal commands (`set`/`stat` a logged-off player) go through an **async bridge** (`GameState.offline_ops` → `game.rs::drain_offline_ops`): load → instantiate → replay the command → save → extract.

## Git

Repo is `/web/deltamud` (its own git repo, `github.com/faratech/deltamud`, branch `main`, pushes straight to origin). Stage explicit paths (`git add rust-mud/src`), not `git add -A` — `lib/` runtime artifacts (`lib/plrobjs/`, `lib/etc/clans.dat`) and `Cargo.lock` are gitignored/untracked and must not be committed. Commit messages end with the `Co-Authored-By:` trailer.
