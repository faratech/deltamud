# DeltaMUD C/Rust Compatibility Guide

This document describes the current compatibility state between the original C
DeltaMUD tree (`/web/deltamud/src`, `/web/deltamud/lib`) and the Rust port in
`/web/deltamud/rust-mud`.

The old early-port guidance in this file is no longer accurate: the Rust port
now has the full command table, most major subsystems, crypt-compatible
password verification, and an 83-column `player_main` mapping. The remaining
risks are mostly runtime fidelity and on-disk persistence compatibility.

The latest tracker-backed parity pass resolved the previously open high-risk
runtime/editor items for complex alias queue timing, creation nanny/do_start,
central OLC save dispatch, DG attachment editing and room `T` saves, social and
wizhelp policy, hostname ban gates, and raw-kill/deathblow side effects.

## Summary

### Generally compatible

- World loaders for zones, rooms, mobiles, objects, shops, help, socials, and
  triggers are broadly implemented.
- SQL player persistence uses the C 83-column `player_main` shape plus
  `player_affects` and `player_skills`.
- Password verification accepts historical DeltaMUD formats, including legacy
  DES crypt, SHA-crypt, and the `pwd_new` bare SHA-256 path.
- The static command table matches C order, and active command handlers are
  wired.

### Not safe to share directly

Do not point the Rust server at production C runtime persistence files without
backups and a deliberate migration/compatibility pass. The high-risk files are:

- Rent/crash player object files under `lib/plrobjs/`
- House object/control files under `lib/house/` and `lib/etc/hcontrol`
- Board message files
- Clan data files
- Mail data files

The C server writes raw structs for several of these formats. The Rust port
uses line-oriented Rust formats for safety and portability. That is easier to
debug, but it is not byte-compatible with the C runtime files.

## SQL Player Data

The Rust `database.rs`/`database_compat.rs` path now targets the original
83-column `player_main` schema. It maps player identity, levels, class/race,
deity, conditions, preferences, combat stats, quest fields, clan fields, arena
fields, map coordinates, god-command bitvectors, affects, and skills.

Player aliases now use the C sidecar format under
`plralias/<bucket>/<lowername>.alias`: alias, replacement, and type triples are
loaded on login and rewritten on alias changes, saves, disconnects, shutdown,
and copyover.

Known SQL/runtime gaps:

- Save currently writes an empty `host` value instead of the descriptor host.
- Played-time accounting appears narrower than C's `played + now - logon`
  update path.
- C's defensive clamps for obviously corrupted gold/bank values are not fully
  mirrored.
- Some offline reporting utilities are not fully SQL-backed yet, including
  autowiz enumeration and parts of clan roster reporting.

## World and Builder Data

World grammar coverage is broad:

- `.zon`: reset commands, `if_flag`, load chance fields, and zone metadata
- `.wld`: room descriptions, exits, special exits, extra descriptions, and room
  trigger attachment load
- `.mob`: act/affect flags, alignment, simple and `X` combat stats, enhanced
  espec blocks, and trigger attachment load
- `.obj`: values, applies, extra descriptions, class/min-level/bitvector fields,
  and trigger attachment load
- `.shp`: shop fields and message order
- `.trg`: trigger prototypes through trigedit/load paths

Known builder/world gaps:

- Several OLC text editors implement only a subset of the C string-editor slash
  commands.
- Mobile prototype special-procedure bindings from OLC are not represented as a
  first-class prototype field; several specs are hardcoded by vnum instead.

## Runtime Fidelity Gaps

The Rust port is no longer an early basic-command prototype. Current
parity gaps are more specific:

- `slist` still emits no spell rows.
- Some `APPLY_*` locations are narrower than C, especially latent fields like
  charisma, age, weight, and height.
- Save currently writes an empty `host` value instead of the descriptor host.
- Played-time accounting appears narrower than C's `played + now - logon`
  update path.
- C's defensive clamps for obviously corrupted gold/bank values are not fully
  mirrored.
- Some offline reporting utilities are not fully SQL-backed yet, including
  autowiz enumeration and parts of clan roster reporting.

## Combat and Gameplay Fidelity Gaps

Known correctness gaps from the latest parity pass:

- No open combat-specific tracker item is currently known from the latest
  parity pass. Continue using the C build as the oracle for exact damage text,
  death side effects, and equipment durability edge cases.

## Safe Operating Guidance

1. Keep the C source as the oracle for behavior fixes.
2. Back up SQL and `lib/` runtime data before any Rust production run.
3. Treat world source files as mostly shareable, but continue comparing Rust
   OLC output against the C build when touching editor text commands or mobile
   special-procedure assignments.
4. Treat runtime persistence files as Rust-only after Rust writes them, unless a
   migration tool is added.
5. Use `MUD_MOCK_DB=true` for local development unless testing SQL behavior.
6. Use `/web/deltamud/bin/circle` side by side when proving exact output,
   combat, editor, or persistence parity.

## Quick SQL Schema Check

```sql
SELECT COUNT(*) AS column_count
FROM information_schema.columns
WHERE table_schema = 'deltamud'
  AND table_name = 'player_main';
```

An 83-column result is the expected C-compatible shape. A much smaller result
indicates an obsolete early Rust schema and should be migrated before use.
