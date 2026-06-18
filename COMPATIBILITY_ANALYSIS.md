# DeltaMUD Rust Implementation - Current Feature Parity Analysis

## Overview

This document supersedes the early Rust-port analysis that described the port as
a small prototype with only basic functionality. That assessment is obsolete.
The Rust tree under `/web/deltamud/rust-mud` now contains the broad DeltaMUD
feature surface: command dispatch, world loaders, SQL player compatibility,
DG scripts, OLC editors, combat, magic, shops, clans, boards, mail, houses,
quests, auction, arena, copyover, and modern client protocol support.

The current state is **near-complete but not exact C parity**. The remaining
work is concentrated in integration, persistence compatibility, editor flows,
and order-sensitive gameplay details. The C source under `/web/deltamud/src`
remains the oracle for behavior.

## Confirmed Implemented Or Broadly Present

- Static command table parity: Rust command names/order match C, including
  sentinels, and active handlers are wired.
- SQL player compatibility: Rust targets the original 83-column `player_main`
  shape plus `player_affects` and `player_skills`.
- Password compatibility: legacy DES crypt, SHA-crypt, and DeltaMUD `pwd_new`
  SHA-256 paths are supported.
- World loading: zones, rooms, mobiles, objects, shops, help/social data, DG
  trigger prototypes, trigger attachments, mob action flags, object applies,
  extra descriptions, and reset load chances are broadly parsed.
- DG runtime: script driver, event queue, trigger prototype editing, mob/object/
  world command sets, boot order, and heartbeat processing are substantially
  implemented.
- OLC/runtime systems: redit/oedit/medit/zedit/sedit/aedit/hedit/trigedit,
  shops, clans, boards, mail, houses, quest, auction, arena, special procs,
  copyover, telnet filtering, GMCP/MSSP, metrics, and panic isolation exist.

## Highest-Priority Remaining Gaps

### Persistence compatibility

Rust SQL player rows are mostly C-shaped, but several C runtime files are not
byte-compatible with Rust:

- C rent/crash object files use raw `rent_info` and `obj_file_elem`; Rust writes
  a line-oriented text format.
- Houses, boards, clans, and mail also diverge from C runtime file layouts.
- Nested-container save order and persisted object fields need review: C stores
  `bitvector`, `curr_slots`, `total_slots`, `min_level`, and affects in raw
  object records; Rust currently omits or zeroes some of these in rent text.
- Rust save paths currently lose descriptor host and likely undercount played
  time compared with C's `played + now - logon` behavior.

Do not share production C runtime persistence files with Rust unless they have
been backed up and intentionally migrated.

### Editor and authoring flows

- The shared string editor return value is ignored, so `/s` completion can fail
  to pop the editor.
- Mail and board composition push raw string editors instead of registering
  completion targets, risking dropped message bodies.
- `write` note authoring validates and prompts but does not enter the object
  action-description editor.
- The C DG attachment-list editor is missing from redit/oedit/medit.
- Room saves drop room `T` trigger attachment lines.
- Central `olc save` does not call every editor's disk writer.
- Several OLC text editors support only a subset of C `string_add()` slash
  commands.

### Character creation and login/runtime policy

- New-character creation skips C nanny states for newbie prompt, deity, hometown,
  stat reroll/accept, and startup initialization.
- Ban enforcement misses C hostname matching, `BAN_NEW`, and `BAN_SELECT`
  behavior.
- C clears active arena/quest runtime state on login; Rust needs parity review
  for that cleanup.
- `-q` currently aliases no-specials in Rust, while C uses only `-s` for
  no-specials.
- `MUD_NO_SPECIALS`/`-s` can be bypassed by lazy special-procedure assignment.

### Combat and gameplay fidelity

- `MOB_WIMPY` is assigned the wrong bit and collides with `MOB_AGGR_EVIL`.
- Sanctuary damage scaling order differs from C.
- Core damage lacks C's low-level non-arena PC-vs-PC guard.
- `deathblow` and immortal raw-kill paths currently route through normal damage.
- Shopkeeper damage protection exists but is not wired into combat.
- Generic equipment handling does not apply armor `value[0]` AC/defense.
- Normal `wear` does not call `wear_otrigger`.
- `slist` prints no spell rows despite spell data now existing.
- Some `APPLY_*` locations are narrower than C, especially charisma, age,
  weight, and height.

### Command and reporting details

- Complex aliases execute expanded commands immediately rather than queuing them
  through descriptor `WAIT_STATE`.
- Aliases are not persisted across sessions.
- Social minimum levels are not enforced.
- `wizhelp` lists commands without filtering by per-command GCMD bits.
- `socials` omits the static `insult` social.
- Autowiz and some clan reporting paths are not fully SQL-backed for offline
  players.

## Near-Term Remediation Order

1. Fix shared editor completion and mail/board/write body persistence.
2. Document or implement a migration boundary for C runtime persistence files.
3. Repair high-impact combat/flag issues (`MOB_WIMPY`, sanctuary order, PvP
   guard, raw-kill/deathblow, armor AC, shopkeeper protection, wear triggers).
4. Restore C character-creation/login policy flows.
5. Wire alias persistence and command-queue expansion semantics.
6. Finish OLC/DG attachment and explicit-save parity.
7. Close remaining display/reporting gaps (`slist`, `wizhelp`, socials,
   autowiz/clan offline data, full string-editor commands).

## Practical Guidance

- Treat `/web/deltamud/src` as the source of truth for behavior.
- Run the C binary (`/web/deltamud/bin/circle`) side by side when proving exact
  output, combat, OLC, or persistence parity.
- Keep backups of SQL and `lib/` before running Rust against shared data.
- World source files are broadly compatible, but avoid saving rooms with Rust
  OLC until room trigger attachment preservation is fixed.
- Track future parity work as concrete behavior bugs, not as missing broad
  subsystems; most broad subsystems are now present.
