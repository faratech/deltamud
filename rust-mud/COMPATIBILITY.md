# DeltaMUD C/Rust Compatibility Guide

This document describes the current compatibility state between the original C
DeltaMUD tree (`/web/deltamud/src`, `/web/deltamud/lib`) and the Rust port in
`/web/deltamud/rust-mud`.

The old early-port guidance in this file is no longer accurate: the Rust port
now has the full command table, most major subsystems, crypt-compatible
password verification, and an 83-column `player_main` mapping. The remaining
risks are mostly runtime fidelity and on-disk persistence compatibility.

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

- Room saves currently drop room `T` trigger attachment lines even though room
  loading reads them.
- The C DG attachment-list editor in redit/oedit/medit is not exposed yet.
- Central `olc save` does not dispatch every editor's disk writer; some editor
  modules save correctly through their own flow but not through the generic
  save command.
- Several OLC text editors implement only a subset of the C string-editor slash
  commands.
- Mobile prototype special-procedure bindings from OLC are not represented as a
  first-class prototype field; several specs are hardcoded by vnum instead.

## Runtime Fidelity Gaps

The Rust port is no longer an early basic-command prototype. Current
parity gaps are more specific:

- Character creation skips several C nanny states: newbie prompt, deity,
  hometown, stat reroll/accept, and creation-time `do_start_init`.
- Shared string editor completion is not fully wired for mail/boards, and
  `write` note authoring does not enter the object text editor.
- Aliases work in memory but are not persisted, and complex alias expansion can
  bypass C `WAIT_STATE` behavior by executing expanded commands immediately.
- Some command display/gating details differ: `wizhelp` does not filter by GCMD
  bits, social minimum level is not enforced, and `socials` omits the static
  `insult` social.
- Ban enforcement does not yet match C's hostname, `BAN_NEW`, and `BAN_SELECT`
  checks.
- `-q` currently behaves like no-specials in Rust; C uses only `-s` for that.

## Combat and Gameplay Fidelity Gaps

Known correctness gaps from the latest read-only parity pass:

- `MOB_WIMPY` is assigned the wrong bit in Rust, colliding with
  `MOB_AGGR_EVIL`.
- Sanctuary damage is scaled in a different order from C.
- Core damage lacks C's low-level non-arena PC-vs-PC guard.
- `deathblow` and immortal raw-kill paths route through normal damage instead
  of matching C's side-effect differences.
- Shopkeeper damage protection exists but is not wired into combat.
- Armor `value[0]` AC/defense is not applied by generic equip handling.
- `wear_otrigger` exists but normal `wear` does not call it.
- `slist` still emits no spell rows.
- Some `APPLY_*` locations are narrower than C, especially latent fields like
  charisma, age, weight, and height.

## Safe Operating Guidance

1. Keep the C source as the oracle for behavior fixes.
2. Back up SQL and `lib/` runtime data before any Rust production run.
3. Treat world source files as mostly shareable, but avoid using Rust OLC to
   save rooms with trigger attachments until room `T` preservation is fixed.
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
