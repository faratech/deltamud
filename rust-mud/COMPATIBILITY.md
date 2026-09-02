# DeltaMUD C/Rust Compatibility Guide

This document describes the current compatibility state between the original C
DeltaMUD tree (`/web/deltamud/src`, `/web/deltamud/lib`) and the Rust port in
`/web/deltamud/rust-mud`.

The old early-port guidance in this file is no longer accurate: the Rust port
now has the full command table, most major subsystems, Argon2id password storage
with historical-format verification, an 83-column `player_main` mapping, and
byte-compatible runtime persistence on the deployed C ABI.

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
  DES crypt, SHA-crypt, and the `pwd_new` bare SHA-256 path. New and changed
  passwords use salted Argon2id PHC strings, and a successful legacy login
  attempts to persist an Argon2id upgrade.
- The static command table matches C order, and active command handlers are
  wired.

### Runtime persistence files (#95 closed)

`src/cformat.rs` holds byte-exact codecs for the C on-disk records, verified
against gcc-computed struct layouts: `rent_info` (56 B) + `obj_file_elem`
(80 B), `house_control_rec` (928 B), `clan_info` (304 B), and board
`board_msginfo` (32 B) + NUL-blob bodies. Player mail was already byte
compatible.

- **Reads:** raw bytes are auto-detected before UTF-8 decoding for C-format
  rent/crash plrobjs, hcontrol, house object files, clans.dat, and boards. The
  prior Rust text/variable-binary/`DBRD` formats remain readable.
- **Writes:** every loaded store retains its detected C or Rust format on
  atomic replacement. `MUD_CFORMAT_FILES=true` (or `1`) selects C only for a
  brand-new file, or for an empty file whose two representations are
  byte-identical; otherwise the existing on-disk format wins. The default for
  such new/ambiguous files remains Rust.
- **Migration:** back up the runtime `lib` tree, set the environment choice,
  and create new files through the desired server. Merely toggling the setting
  does not convert existing files. An empty house object file and a zero-clan
  `clans.dat` contain no format signature, so a cold boot necessarily uses the
  configured default for those two cases.

## SQL Player Data

The Rust `database.rs`/`database_compat.rs` path now targets the original
83-column `player_main` schema. It maps player identity, levels, class/race,
deity, conditions, preferences, combat stats, quest fields, clan fields, arena
fields, map coordinates, god-command bitvectors, affects, and skills.

Authority-changing operations use a narrow compare-and-swap over the complete
persisted authority tuple (level, trust, and all four GCMD bitvectors). The live
character is updated only after a committed result or exact durable readback;
an ambiguous outcome quarantines the identity so it cannot exercise staff
authority until reconciliation. Generic player saves cannot publish a pending
authority or password change.

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

## Modern operational behavior (not C file-format parity)

These controls intentionally modernize deployment and security without changing
the C world grammar or command-oracle policy:

- MySQL has no compiled-in credential and is never selected by falling back to
  an unsafe URL. Debug/test builds may default to the ephemeral mock backend;
  release builds default to real MySQL, which requires an explicit non-empty
  `DATABASE_URL`. Operators should always set `MUD_MOCK_DB` explicitly.
- The real schema is an ordered, checksummed migration set. Normal server boot
  verifies it and its authorization-sensitive storage shape (including player
  name collation and level type) and fails closed; only the offline
  `deltamud --migrate` mode may apply it. Loading independently rejects a
  malformed player level instead of allowing a signed value to wrap into an
  immortal `u8` level.
- Creating idnum 1 no longer creates an administrator. The first character is a
  level-1 mortal. A new installation may promote one existing durable player
  through the one-time offline `--bootstrap-implementor <name>` mode, which
  refuses to run after an Implementor exists.
- OLC world-file replacement is crash-conscious: sibling temp file, checked
  write/flush/fsync, atomic rename, and directory fsync. Disk-first editors
  publish live state only after disk success; REDIT/OEDIT keep the C two-stage
  memory-then-`olc save` model. Save-list completion is published only after a
  durable write. Manual save, shutdown, auto-reboot, and copyover all abort
  their success/exit/exec path when an outstanding entry cannot be made
  durable.
- Every authority-bearing editor retains the exact authenticated session tuple
  and revalidates persisted trust, quarantine, GCMD grants, and the current
  zone ACL at its publication boundary. Revocation while an editor is open
  therefore discards or retains scratch work without publishing it.
- New-zone creation writes a versioned durable marker before publishing any of
  its six components or indexes. Boot hides every indexed component for a
  marked zone and restores the shutdown/copyover blocker; an exact idempotent
  `zedit new` retry completes the publication and removes the marker.
- Mail consumption is a whole-store copy-on-write replacement. A partial
  rewrite cannot mark only part of a message deleted or make the original
  unreadable.
- Staff command authority belongs to direct input from the exact authenticated
  player principal. `force`, `order`, and DG commands may still drive ordinary
  gameplay, but cannot spend a staff principal's trust or GCMD capabilities.
- Fresh connections receive server-initiated `WILL GMCP`; negotiated descriptors
  retain bounded `Core.Hello` and `Core.Supports.Set/Add/Remove` capability state
  and receive `Char.Vitals`/`Room.Info`. Plain Telnet and MSSP remain compatible,
  and clients that do not negotiate GMCP receive no GMCP payload.
- UTF-8 negotiation/input policy, NAWS, TTYPE/MTTS, MCCP, `Room.Add/Remove`, and
  `Char.Items` are deliberately deferred rather than partially advertised.

## Divergence Register (deliberate deviations from the C oracle)

Policy: where the C oracle itself is buggy, the Rust port implements the
correct behavior and records it here. Gameplay fidelity otherwise follows the C
oracle, apart from the explicit operational/security controls above.

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
| `appraise` command | Finish-the-game activation | C defines `do_value`/shop value logic but its cmd_info[] row for "value" points at a dead `do_not_here` stub and no "appraise" row exists, so neither is reachable | `appraise` is registered (Standing, mortals) and routes to the shop value handler | Architecture roadmap P5 |
| Corrupt board file | Data-loss repair | Logs "Board file corrupt. Resetting.", loads an empty board, and lets the next save unlink/overwrite the unreadable file (boards.c Board_load_board) | The corrupt file is quarantined: boot logs an error, the board loads empty, and every save for that board is refused so the file survives for operator recovery | Architecture roadmap P0 |
| Name-based privilege backdoors | Removed | Public `levelme` promotes the exact name `Mulder`; `snoop` bypasses hierarchy for that name; zone builder lists use prefix matching | `levelme` is not dispatchable, `snoop` applies one principal rule to every name, and builder ACLs use exact case-insensitive tokens whose identities cannot be newly reused while referenced | Security modernization |

### Finish-the-game completions (in progress, 2026-08)

Per the approved completion roadmap, previously-dead C systems are being activated.
Each activation is a registered, intentional divergence from the shipped C binary:

| Feature | C oracle state | Port state |
|---|---|---|
| `copy` / `rlink` builder commands | Complete in olc.c but never registered in cmd_info — unreachable | Registered (`copy`/`rlink`, GOD_CMD2/OLC). C's never-firing object-target guard placed after type parse; rlink's unreachable "no space in zone" guard repaired; rlink disconnect NULL-exit deref guarded |
| `lweather` admin console | Unconditional hex-echo + return above the whole subcommand chain | Subcommands (`update_weather_activity`, `update_weather_map`, `new`, `destroy`, fallthrough `listweather`) are live; non-subcommand arguments still get C's hex echo. The abandoned gmode experiment and the interactive `edit` walker are not carried |
| `togglemap off` | Early return + "NO! YOU'LL HURT SOMEONE!" before unload_map | Kept as shipped — the author's guard is a deliberate choice, not a bug |
| `show_on_who_list` (builders visible in `who`) | Function written ("future expansion capabilities") but its call site commented out; builders hidden | No change needed: the port's who walks in-world players, so builders are visible — the author's intent already holds |

### Finish-the-game Wave 2 (practices + quests)

| Feature | C oracle state | Port state |
|---|---|---|
| `SPECIAL(guild)` / `guild_guard` | Declared, never ASSIGNMOB'd; `guild_info[]` points at stock Midgaard rooms that never shipped — players could never spend a practice | Assigned to authored Itrius guildmasters (mobs 115–119) and entrance guards (126–130); `GUILD_INFO` retargeted to the real guild entrances (Mage←123s, Cleric←103s, Thief←115n, Warrior←119n, Artisan←106n; the Artisan row is an addition, C had none) |
| Quest reward marker | Clamps `estimate_difficulty` to ≥1 BEFORE /5, so a same-level target yields -0 == 0 and the marker is wiped (quest uncompletable) | Clamp applied after the division: `(difficulty/5).max(1)` |
| Questmaster + targets + rewards | No mob carries MOB_QUESTMASTER/MOB_QUEST; the 12 reward objects and 5 tokens (9002…9010, 3082, 3160, 3161, 3163, 6814, 8620, 18702) were never authored | Authored at the C-hardcoded vnums with stub vault zones; 21 quest targets flagged across zones 11/14/16/20/21; questmaster mobs 120 (Itrius) and 2150 (zone 21). Reward display name "Midgaard Hero Vest" re-skinned to "Itrius Hero Vest" (vnums, order and QP prices preserved); authored mobs are SENTINEL-flagged |
| Mob hunting | DeltaMUD dropped stock CircleMUD's `if (HUNTING(ch)) hunt_victim(ch);` driver — DG `mhunt` set HUNTING and nothing consumed it | Stock driver restored in mobile_activity after the awake/fighting gates, ending the mob's turn |

### Finish-the-game Waves 5-7 (world completion, magic, web/ops)

| Feature | C oracle state | Port state |
|---|---|---|
| Zones 21/22/11/15 placeholders | 132 rooms shipped as "You are in an unfinished room."; zone 15 titled with the builder's goodbye note | All finished in each zone's voice, exits bidirectionally consistent, all 11 DG trigger attachments preserved; zone 15 retitled "Jarik's Watch" |
| The missing middle (L31-99) | Nothing between mob level 30 and two L100 town NPCs | Zone 30 Sundered Marches (L30-50), 31 Sunken Cloister (L45-70), 32 Ashen Spire (L60-99): 74 rooms, 31 mobs (4 MOB_QUEST-flagged per zone), chained 3044->3101->3200 |
| SPELL_TELEPORT / SPELL_GROUP_RECALL | Handlers exist (spells.c:173 / magic.c:559) but never spello()'d; teleport had an inverted NULL check | Both registered with mortal level rows (MAG 46 / CLE 49); teleport excludes synthetic surface-map cells; the C NULL-check bug is fixed by construction in the port |
| Web who-list (www_who) | make_who2html complete but gated behind a broken `if (!(www_who) > 0)` guard, a hardcoded /home/mulder path, a system("mv") shell-out, and an unregistered whoupd command | Native whohtml.rs: same page, configurable MUD_WWW_WHO_DIR, atomic tmp+rename, driven by www_who (MUD_WWW_WHO=1) from the heartbeat autosave block; whoupd live with C's rewww kept as alias |
| Auto-reboot clock | setreboot armed a schedule (reboot_hr/min, warn_hr/min) that nothing consumed | The heartbeat consumes it: warning broadcast at the warn time, then save-all + OLC flush + graceful exit 75 at reboot time (`MUD_AUTOREBOOT=1`), which the supplied systemd unit restarts. Intentional `shutdown die/pause` and SIGTERM exit 0 and stay stopped. |
| pt_markable | Shipped NO: theft allowed, THIEF branding dead | Implemented behind MUD_PT_MARKABLE=1 (default matches the oracle) |
| zone.lst | The stock CircleMUD fantasy catalog (54 zones, zero shipped) | Rewritten to the real 25-zone catalog |
| Mage Guild (room 156), zone-6 room 601, 16.wld #1631 | Placeholder text | Finished |
| 1.mob / 48.wld block order | Unsorted — breaks the legacy C loader's positional binary searches | Blocks sorted into vnum order (behavior-preserving; repairs 19 pre-existing C-side resolution failures) |
| Deities | 15 selectable deities, zero mechanical consumers | Kept as flavor by design; documented |

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
4. Preserve the auto-detected format of existing runtime files.
   `MUD_CFORMAT_FILES=true` selects C format only for new or intrinsically
   ambiguous empty stores; toggling it does not convert existing files.
5. Set `MUD_MOCK_DB=true` explicitly for local ephemeral development. For real
   MySQL, set `MUD_MOCK_DB=false` plus an explicit `DATABASE_URL`, run
   `deltamud --migrate` offline, and never assume normal boot will modify schema.
6. Create the initial Implementor only with the one-time offline
   `--bootstrap-implementor <name>` workflow; creating the first character is
   intentionally not an authorization mechanism.
7. Use `/web/deltamud/bin/circle` side by side when proving exact output,
   combat, editor, or persistence parity.

## Quick SQL Schema Check

The binary is the authoritative verifier: with the real backend selected, a
normal start checks the migration names/checksums and refuses a mismatched
schema. For an operator-visible inventory:

```sql
SELECT version, name, checksum
FROM schema_migrations
ORDER BY version;
```

The current binary expects versions 1 through 4. The original shape check is
still useful when importing an older database:

```sql
SELECT COUNT(*) AS column_count
FROM information_schema.columns
WHERE table_schema = 'deltamud'
  AND table_name = 'player_main';
```

An 83-column result is the expected C-compatible shape. A missing migration
ledger or a much smaller result indicates an obsolete schema; back it up and run
the explicit offline migration rather than allowing normal game startup.

## Deltania Breathes — spec-assignment collisions (2026-09-01)

C's spec_assign.c assigns specs to Midgaard vnums that OUR world reuses for
new content. Left alone they misbehave (not crashes — the recursion behind
mob 3105 was fixed separately in 3bc3522):

| Assignment (C) | C intent | Our vnum now | Action |
|---|---|---|---|
| `assign(3105, mayor)` | Midgaard mayor patrol | zone 31 drowned templar (mob/31.mob) | removed — patrol walked Cloister mobs |
| `assign(3031, pet_shops)` | Midgaard pet-shop room | zone 30 "The Tower Magazine" | removed — pet_room = in_room+1 arithmetic pointed at a random room |
| `assign(3060/3067, cityguard)`, `assign(3061, thief)`, `assign(3062, fido)`, `assign(3095, magic_user)` | Midgaard mobs | no mob with those vnums is loaded | kept (inert; no proto resolves) |
