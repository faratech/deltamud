# Contributing to DeltaMUD Rust

DeltaMUD Rust is a compatibility-conscious modernization of the original
CircleMUD-derived game. Review `AGREEMENT` before contributing; the project's
licensing terms are more restrictive than a normal OSI open-source license.

## Development contract

- Treat `src/` as the read-only C behavior oracle for unchanged classic systems.
- Register intentional Rust behavior changes in `rust-mud/COMPATIBILITY.md` and
  protect them with tests.
- Preserve the single-owner `GameState`; async work belongs at bounded edges.
- Keep plain-text play complete even when adding structured client data.
- Make persistence fail closed. Never clear dirty/pending state after a failed
  durable write.
- Do not add secrets, production runtime files, or player data to the repository.

## Required checks

Run these commands from `rust-mud/`:

```bash
cargo fmt --all -- --check
cargo check --all-targets --locked
cargo test --locked
cargo test --locked -- --test-threads=1
scripts/clippy-check.sh
cargo build --release --locked
scripts/systemd-check.sh
```

Changes to gameplay, persistence, protocol, deployment, or world data also run
the relevant database, parity, balance, and isolated canary gates from
`rust-mud/docs/RUNBOOK.md`.

Use `rustfmt` for Rust formatting. Keep commits narrowly scoped, explain the
player-visible behavior, and include the test or transcript that would fail if
the bug returned.

The Clippy gate fails on compiler warnings and every lint category not listed in
`scripts/clippy-check.sh`. That explicit category list is inherited cleanup debt,
not a target: remove entries as the port is modernized, and do not add a category
to hide a new finding.
