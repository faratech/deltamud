# DeltaMUD

DeltaMUD is a text-based multiplayer RPG (MUD) virtual world engine derived from CircleMUD 3.0.

The repository contains two implementations of the same game:

- A modern **Rust engine** in [`rust-mud/`](rust-mud/README.md) — the actively developed and supported server. It boots against the original world files in `lib/` and aims for behavioral parity with the C original.
- The **legacy C codebase** in `src/` — the historical CircleMUD-derived reference implementation. It is retained as the behavior oracle for the Rust port and, per [`SECURITY.md`](SECURITY.md), is not a supported public production target.

Gameplay systems present across the codebase include combat, magic and skills, shops, clans, boards, mail, houses, quests, auction, an arena, DG Scripts (trigger-based scripting), and OLC (online creation) editors. For the detailed feature list and architecture, see [`rust-mud/README.md`](rust-mud/README.md).

## Rust engine vs. legacy C

| Area | Rust engine (`rust-mud/`) | Legacy C (`src/`) |
| --- | --- | --- |
| Status | Actively developed; supported production target | Historical reference / behavioral oracle |
| Entry point | `deltamud` binary built by Cargo | `bin/circle` built by `make` (see Legacy C below) |
| World data | Loads the shared files under `lib/` | Loads the same files under `lib/` |
| Player persistence | MySQL/MariaDB (`player_main`, `player_affects`, `player_skills`) or an in-memory mock backend for development | MySQL via `src/dbinterface.c` |
| Documentation | [`rust-mud/README.md`](rust-mud/README.md), [`rust-mud/docs/RUNBOOK.md`](rust-mud/docs/RUNBOOK.md) | [`CLAUDE.md`](CLAUDE.md) (repo root) |

The C source is the source of truth for exact behavior. When proving parity, run the C and Rust servers side by side and compare output. See [Compatibility](#compatibility).

## Quick start (Rust engine)

### Prerequisites

- **Rust 1.98.0.** The toolchain is pinned in [`rust-mud/rust-toolchain.toml`](rust-mud/rust-toolchain.toml) (`channel = "1.98.0"`, with `clippy` and `rustfmt` components) and declared as `rust-version = "1.98"` with `edition = "2024"` in [`rust-mud/Cargo.toml`](rust-mud/Cargo.toml). If you use rustup, it selects the pinned toolchain automatically inside `rust-mud/`.
- **MySQL / MariaDB — optional for a first run.** No database is needed when the in-memory mock backend is selected explicitly (see below). A real database is required only for durable player data.
- Dependencies are pinned in `rust-mud/Cargo.lock`; pass `--locked` to dependency-resolving Cargo commands, as the project docs and CI do.

### Build

From the repository root:

```bash
cd rust-mud
cargo build --release --locked
```

This produces the server binary at `rust-mud/target/release/deltamud`. The helper script [`rust-mud/build.sh`](rust-mud/build.sh) runs the same release build.

### Run locally without MySQL (mock database)

From `rust-mud/`, start the server against the shared world files with the ephemeral in-memory backend:

```bash
MUD_MOCK_DB=true MUD_BIND=127.0.0.1 MUD_PORT=4000 \
  MUD_LIB_PATH=../lib ./target/release/deltamud
```

Notes:

- `MUD_MOCK_DB=true` selects the in-memory mock database; its state does not survive a restart. Always set `MUD_MOCK_DB` explicitly — its default is build-dependent (debug/test builds default to mock, release builds default to the real backend).
- `MUD_LIB_PATH` points at the world/data directory. Run from `rust-mud/`, the shared world is `../lib` (the repository-root `lib/`). The compiled-in default is `./lib`.
- Do not point two running servers at the same `MUD_LIB_PATH` directory; rent and runtime-state writes under it would race. Use a private copy of `lib/` for any second instance.

### Connect and play

With the server running, connect with any Telnet or MUD client:

```bash
telnet 127.0.0.1 4000
```

`nc` also works for scripted checks. The default game port is **4000** (`MUD_PORT`). Plain Telnet remains a complete way to play; the Rust server additionally negotiates modern client protocols (GMCP, MSSP) with clients that support them. See [`rust-mud/README.md`](rust-mud/README.md) for details.

The first character you create is an ordinary level-1 mortal. To promote a character to Implementor on a local durable (MySQL-backed) database, see [First Implementor](#first-implementor-durable-database-only).

## Configuration

The Rust server is configured through environment variables (read in `rust-mud/src/config.rs` / `main.rs`). The most important ones for getting started:

| Variable | Default | Purpose |
| --- | --- | --- |
| `MUD_BIND` | `0.0.0.0` | Game listener address. Use `127.0.0.1` for local-only development. |
| `MUD_PORT` | `4000` | Game listen port. |
| `MUD_LIB_PATH` | `./lib` | World/data and writable runtime-state directory. |
| `MUD_MOCK_DB` | build-dependent | `true` = ephemeral in-memory backend; `false` = MySQL. Invalid values fail startup. |
| `DATABASE_URL` | *(none)* | Required and non-empty whenever the real (MySQL) backend is selected. There is no compiled-in credential or automatic fallback. |
| `MUD_DB_TIMEOUT_SECS` | `5` | Timeout applied to each database operation. |
| `MUD_METRICS_PORT` | *(off)* | When set, enables `/metrics`, `/live`, `/ready`, `/health`, and `/api/who` endpoints. |
| `RUST_LOG` | `info` | Log level. |

This is a starter subset. The full variable reference — including connection limits, reverse-DNS, copyover (`MUD_EXEC_PATH`), and no-specials mode — is in [`rust-mud/README.md`](rust-mud/README.md#configuration-environment-variables) and [`rust-mud/docs/RUNBOOK.md`](rust-mud/docs/RUNBOOK.md). An example production environment file is provided at [`rust-mud/deploy/systemd/deltamud.env.example`](rust-mud/deploy/systemd/deltamud.env.example); never commit a populated `DATABASE_URL`.

Command-line modes other than normal boot:

- `./target/release/deltamud --migrate` — initialize or upgrade the database schema, then exit (see below).
- `./target/release/deltamud --bootstrap-implementor <Name>` — one-time offline Implementor promotion (see below).
- `-s` (or `MUD_NO_SPECIALS`) — skip special-procedure assignment, matching the C `-s` flag.

## Database setup

### Development: no database

Use `MUD_MOCK_DB=true` as in the quick start. Nothing to install or migrate; all player state is ephemeral.

### Durable: MySQL / MariaDB

Player persistence uses three tables — `player_main` (83 columns), `player_affects`, and `player_skills`. The workflow is:

1. Create a MySQL/MariaDB database (conventionally named `deltamud`) and a user with access to it.
2. Build the server (above), then run migrations **before** the first normal boot. Normal startup verifies the schema but never creates or alters tables:

   ```bash
   MUD_MOCK_DB=false DATABASE_URL="mysql://deltamud:<pw>@127.0.0.1/deltamud" \
     ./target/release/deltamud --migrate
   ```

3. Start the server with the same `DATABASE_URL` and `MUD_MOCK_DB=false`.

Schema sources in this repository:

- [`rust-mud/migrations/`](rust-mud/migrations/) — the checksummed migrations (`0001`–`0004`) applied by `deltamud --migrate`, which owns the `schema_migrations` ledger. This is the production path.
- [`deltamud_schema.sql`](deltamud_schema.sql) — an empty compatibility snapshot of the same table shapes for tooling and the C oracle. As its header states, production Rust deployments must run `deltamud --migrate` rather than applying this file. It seeds no administrative identity or password.

Back up the database and the `lib/` tree before running the Rust server against data shared with the C server. Runtime persistence-file compatibility and migration caveats are covered in [`rust-mud/COMPATIBILITY.md`](rust-mud/COMPATIBILITY.md).

### First Implementor (durable database only)

Create the character normally in-game first, stop the server, then run the one-time offline promotion:

```bash
MUD_MOCK_DB=false DATABASE_URL="mysql://deltamud:<pw>@127.0.0.1/deltamud" \
  ./target/release/deltamud --bootstrap-implementor Founder
```

The target must already be a durable player. The command exits without starting the listener and refuses to run if an effective Implementor already exists or another server/maintenance command owns the same database. Production deployments use the trusted release manager's bootstrap workflow instead — see [`rust-mud/docs/RUNBOOK.md`](rust-mud/docs/RUNBOOK.md).

## Project layout

```text
.
├── README.md                  This file
├── AGREEMENT                  Project agreement / licensing terms (read before use)
├── COMPATIBILITY_ANALYSIS.md  Root-level Rust/C feature-parity analysis
├── CONTRIBUTING.md            Contribution contract and required checks
├── SECURITY.md                Vulnerability reporting and supported code
├── CLAUDE.md                  Legacy-C build/run notes and architecture overview
├── deltamud_schema.sql        Compatibility snapshot of the MySQL schema (see Database setup)
├── rust-mud/                  Rust engine (supported server)
│   ├── Cargo.toml             Package `deltamud`, edition 2024, rust-version 1.98
│   ├── rust-toolchain.toml    Pinned toolchain 1.98.0 (clippy, rustfmt)
│   ├── src/                   Engine source (single-owner GameState; async only at the socket edge)
│   ├── migrations/            Checksummed SQL migrations applied by `deltamud --migrate`
│   ├── scripts/               Clippy gate, parity/canary checks, playthrough and release tooling
│   ├── deploy/systemd/        Hardened systemd units and environment template
│   ├── docs/RUNBOOK.md        Operational runbook (build, boot, deployment, gates)
│   ├── COMPATIBILITY.md       C/Rust compatibility guide and divergence register
│   └── CLAUDE.md              Rust development guidance (architecture deep-dive)
├── src/                       Legacy C source (CircleMUD-derived reference/oracle)
├── lib/                       Shared world files and runtime state (see below)
├── bin/                       Prebuilt legacy C binaries and utilities
├── cnf/                       Legacy build configuration inputs
└── autorun                    Legacy C auto-restart script (see Legacy C)
```

### World files (`lib/`)

Both engines load the shared world under `lib/`:

- `lib/world/wld/` — room files (`*.wld`)
- `lib/world/mob/` — mobile (NPC) files (`*.mob`)
- `lib/world/obj/` — object files
- `lib/world/zon/` — zone files (`*.zon`) and reset definitions
- `lib/world/shp/` — shop files
- `lib/world/trg/` — DG trigger files
- `lib/world/zone.lst` — index of zone numbers in use. The historical notes in `lib/world/README` describe the original CircleMUD world archive — leave area credits in zone files intact.
- `lib/etc/` — server configuration and runtime state (for example `players`, `clans.dat`, `badsites`)
- `lib/text/` — text shown to players (`motd`, `imotd`, `help/`, `background`, `credits`, and similar)
- `lib/misc/` — auxiliary game data (for example `messages`, `socials`)
- `lib/plrobjs/`, `lib/plredata/` — per-player object/runtime data written at runtime

Treat `lib/` as writable runtime state for a running server, not as read-only source: back it up before parity experiments or migrations, and give each simultaneously running server its own copy.

## Development and testing

All commands below run from `rust-mud/` and are taken from [`CONTRIBUTING.md`](CONTRIBUTING.md) and [`rust-mud/README.md`](rust-mud/README.md):

```bash
cargo fmt --all -- --check
cargo check --all-targets --locked
cargo test --locked
cargo test --locked -- --test-threads=1
scripts/clippy-check.sh
cargo build --release --locked
scripts/systemd-check.sh
```

Additional checks documented in the runbook include the live playthrough, parity, and canary gates:

```bash
PYTHONDONTWRITEBYTECODE=1 python3 scripts/playthrough_test.py -v
scripts/parity-check.sh   # compares C and Rust servers on acknowledged transcripts
scripts/canary.sh         # isolated-state semantic health/combat checks
```

See [`rust-mud/docs/RUNBOOK.md`](rust-mud/docs/RUNBOOK.md) for which gate applies to a given change. Contribution rules — including treating `src/` as the read-only C oracle, registering intentional behavior changes in `rust-mud/COMPATIBILITY.md`, and never committing secrets or player data — are in [`CONTRIBUTING.md`](CONTRIBUTING.md). CI is defined in `.github/workflows/rust-mud-ci.yml` (formatting, build, tests, clippy, MariaDB persistence, and bounded live canaries).

## Compatibility

The Rust port aims for behavioral parity with the C original: matching output strings, world-file formats, numeric formulas, and the 83-column `player_main` schema. The broad DeltaMUD feature surface is implemented, and remaining work is concentrated in integration, persistence compatibility, editor flows, and order-sensitive gameplay details rather than missing subsystems.

This README deliberately does not reproduce the full audit. Read instead:

- [`COMPATIBILITY_ANALYSIS.md`](COMPATIBILITY_ANALYSIS.md) — root-level summary of confirmed-implemented areas, highest-priority remaining gaps, and remediation order.
- [`rust-mud/COMPATIBILITY.md`](rust-mud/COMPATIBILITY.md) — the detailed C/Rust compatibility guide, runtime persistence-file formats, and the register of deliberate divergences where the Rust port repairs a C bug.

Practical guidance from those documents: back up SQL and `lib/` before running Rust against shared data, and do not share production C runtime persistence files with Rust unless they have been backed up and intentionally migrated.

## Legacy C

The C server is retained as the compatibility oracle. Per [`CLAUDE.md`](CLAUDE.md) and [`SECURITY.md`](SECURITY.md), it is not a supported public production target; prefer the Rust engine for any new deployment.

For reference, the legacy build and run flow documented in [`CLAUDE.md`](CLAUDE.md) is:

```bash
# Build (generates bin/circle)
cd src && make clean && make all

# Debug/test run on the default port 4000 (from the repository root)
bin/circle -q 4000

# Or production-style auto-restart (from the repository root)
./autorun
```

Other documented legacy invocations include `bin/circle -c` (configuration/syntax check) and `bin/circle -m 4000` (mini-Mud mode with limited zones). The default C port is also 4000, the data directory is `lib/`, and the MySQL database is `deltamud` (see [`deltamud_schema.sql`](deltamud_schema.sql)).

## Contributing and security

- Contributions: start with [`CONTRIBUTING.md`](CONTRIBUTING.md) and review [`AGREEMENT`](AGREEMENT) first — the project's licensing terms are more restrictive than a normal OSI open-source license.
- Security issues: do not open a public issue for vulnerabilities affecting credentials, player data, administrative access, or a running server. Follow [`SECURITY.md`](SECURITY.md) and use the repository's private GitHub Security Advisory form.

## License

DeltaMUD is a derivative of CircleMUD 3.0, and the original CircleMUD and DikuMUD license doctrines apply.

The project's own terms are recorded in [`AGREEMENT`](AGREEMENT). In summary, by downloading and running this software you agree to adhere to the CircleMUD and DikuMUD license agreements, not to use the software for commercial gain, and not to pass off the authors' creative work as your own; the creators disclaim responsibility for damage arising from running, installing, or implementing the software; and the DeltaMUD, CircleMUD, and DikuMUD contributors must be acknowledged. `AGREEMENT` is the authoritative text — this summary does not replace it. Note that these terms are more restrictive than a typical OSI open-source license (notably the non-commercial condition).

The Rust package declares its license via `license-file = "../AGREEMENT"` in [`rust-mud/Cargo.toml`](rust-mud/Cargo.toml). Credits: thanks to the original DeltaMUD team, the CircleMUD creators, and the Rust ecosystem (see also `lib/text/credits`).
