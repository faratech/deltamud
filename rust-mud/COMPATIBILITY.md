# DeltaMUD C/Rust Compatibility Guide

This document describes the current compatibility state between the original C
DeltaMUD tree (`/web/deltamud/src`, `/web/deltamud/lib`) and the Rust port in
`/web/deltamud/rust-mud`.

The old early-port guidance in this file is no longer accurate: the Rust port
now has the full command table, most major subsystems, crypt-compatible
password verification, and an 83-column `player_main` mapping. The remaining
high-risk caveat is on-disk runtime persistence compatibility.

The latest tracker-backed parity pass resolved the previously open high-risk
runtime/editor items for complex alias queue timing, creation nanny/do_start,
central OLC save dispatch, DG attachment editing and room `T` saves, social and
wizhelp policy, hostname ban gates, raw-kill/deathblow side effects,
descriptor-host SQL saves, played-time accounting, corrupted gold/bank clamps,
`slist`, broader `APPLY_*` handling, SQL-backed autowiz/clan roster reporting,
MOB_CASTER magic-user binding, and C-style slash-command handling in inline OLC
text buffers.

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

### Runtime persistence files (#95 closed)

`src/cformat.rs` holds byte-exact codecs for the C on-disk records, verified
against gcc-computed struct layouts: `rent_info` (56 B) + `obj_file_elem`
(80 B), `house_control_rec` (928 B), `clan_info` (304 B), and board
`board_msginfo` (32 B) + NUL-blob bodies. Player mail was already byte
compatible.

- **Reads:** C-format files are auto-detected and loaded (rent/crash plrobjs,
  hcontrol, clans.dat, boards). Rust text formats remain readable.
- **Writes:** setting `MUD_CFORMAT_FILES=true` makes the Rust server WRITE the
  C formats; the default remains the Rust text formats. Do not mix writers on
  the same live files without backups (a file written by one format family is
  still readable by the other at boot, but prefer one writer).

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

- No known SQL/runtime fidelity gap remains from the latest tracker-backed
  audit. Runtime persistence file byte-compatibility remains separate below.

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

- No known builder/world fidelity gap remains from the latest tracker-backed
  audit. Inline OLC text buffers share the generic runtime `modify.rs` parser
  for the C-style string-editor slash command set.

## Divergence Register (deliberate deviations from the C oracle)

Policy: where the C oracle itself is buggy, the Rust port implements the
correct behavior and records it here. Everything else matches the C.

| Area | Divergence | C behavior (oracle) | Rust behavior | Tracker |
|---|---|---|---|---|
| DG `mjunk` | Branch inversion repaired | FIND_INDIV runs the "all" loop with arg+4; "all"/"all.x" extracts ONE item (dg_mobcmd.c:196-217) | The intended per-item / all branches work | #152 |
| DG `wait 0` | Robustness | `--time == 0` becomes -1 and the trigger wedges forever (dg_event.c:82) | `wait 0` fires on the next pulse | #157 |
| DG `wat` | Correct addressing | Passes the vnum where an rnum is required, so the wrong room is affected whenever vnum != rnum (dg_wldcmd.c:572-579) | Resolves the room correctly | #162 |
| `stat`/`score` Played | Arithmetic repair | Prints `(played/3600) % 60` as minutes, so >60 h wraps (act.wizard.c:867) | Correct h/m arithmetic | #214 |
| zedit ZONE_TOP clamp | Uses the builder zone number | Clamps with the zone-table INDEX (`OLC_ZNUM * 100`, zedit.c:1722) | Clamps with the zone number (the intent) | #285 |
| sedit new-product abort | Menu stays in context | The -1 abort path shows the ROOMS menu (a copy-paste slip, sedit.c:1176) | Re-shows the products menu | #296 |
| aedit numeric fields | Clamped | An `&&` of mutually-exclusive tests makes the range check unreachable, so ANY integer is stored and later indexes tables out of range (aedit.c:601/616) | Clamped to the legal range | #297 |
| medit/sedit numeric prompts | Stricter guard | Only `""`/`-<nondigit>` are rejected, so `abc` applies atoi()==0 (medit.c:893, sedit.c:897) | Non-numeric input re-prompts; nothing is stored | #302 |
| `build off` | Reachable | `real_room(atoi("off")) < 0` runs first, so C ALWAYS rejects `build off` as a bad room (act.other.c:328-335) | `build off` performs the off action | #320 |
| redit special-exit `-1` | Menu stays in context | After clearing a special-exit destination, C re-displays the regular exit menu (a copy-paste slip at redit.c:1210) | Re-displays the special-exit menu | #268 |
| Multiplay gate | Matches shipped C | `check_multiplaying` begins with `return 1` ("development mode"), so multi-boxing is never blocked (comm.c:2749) | Same default; the full C counting logic is live behind `MUD_ENFORCE_MULTIPLAY=1` | #219 |

### Finish-the-game completions (in progress, 2026-08)

Per the approved completion roadmap, previously-dead C systems are being activated.
Each activation is a registered, intentional divergence from the shipped C binary:

| Feature | C oracle state | Port state |
|---|---|---|
| `copy` / `rlink` builder commands | Complete in olc.c but never registered in cmd_info — unreachable | Registered (`copy`/`rlink`, GOD_CMD2/OLC). C's never-firing object-target guard placed after type parse; rlink's unreachable "no space in zone" guard repaired; rlink disconnect NULL-exit deref guarded |
| `lweather` admin console | Unconditional hex-echo + return above the whole subcommand chain | Subcommands (`update_weather_activity`, `update_weather_map`, `new`, `destroy`, fallthrough `listweather`) are live; non-subcommand arguments still get C's hex echo. The abandoned gmode experiment and the interactive `edit` walker are not carried |
| `togglemap off` | Early return + "NO! YOU'LL HURT SOMEONE!" before unload_map | Kept as shipped — the author's guard is a deliberate choice, not a bug |
| `show_on_who_list` (builders visible in `who`) | Function written ("future expansion capabilities") but its call site commented out; builders hidden | No change needed: the port's who walks in-world players, so builders are visible — the author's intent already holds |

## Runtime Fidelity Gaps

The 2026-08 parity program (GitHub issues #96-#347, epic #348) audited all 12
subsystem buckets function-by-function against the C oracle. All confirmed
gaps are closed; remaining items are the deliberate divergences registered
above. Continue using the C build (`/web/deltamud/bin/circle`) as the oracle
for exact output text when porting anything new.

## Safe Operating Guidance

1. Keep the C source as the oracle for behavior fixes.
2. Back up SQL and `lib/` runtime data before any Rust production run.
3. Treat world source files as mostly shareable, and compare Rust OLC output
   against the C build when changing save grammars or editor behavior.
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
