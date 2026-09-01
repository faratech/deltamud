# DeltaMUD modernization program

The architectural target is **classic core, modern edge**. DeltaMUD keeps its
deterministic single-owner simulation, existing content and characters, MySQL,
DG Scripts, and explicit C compatibility. New behavior is introduced through
negotiated client capabilities, versioned data contracts, and reversible
migrations.

## Decisions

- Mudlet is the first enhanced client; browser access follows the protocol and
  deployment foundation.
- Plain Telnet remains a complete compatibility experience.
- Hardened systemd on the current host is the initial deployment target.
- Accounts are designed now but implemented after deployment and protocol
  stability; legacy character login remains during migration.
- Procedural dungeons and instance primitives are deferred into a separate epic.
- MySQL and DG Scripts remain. This program does not introduce SQLite, Lua,
  ECS, microservices, or an asynchronous actor-owned world.
- `rustfmt` is the canonical Rust formatter.

## Delivery order

1. Project truth, durable writes, authentication, locked builds, and schema
   discipline.
2. Non-root versioned releases, readiness, rollback, backup, and restore.
3. Stateful Telnet capability negotiation, UTF-8, and richer GMCP.
4. Typed synchronous game-event projections and an official Mudlet package.
5. Persistence-worker, module, builder, DG Script, and content-publication
   improvements.
6. Staged accounts, browser access, community systems, and measured gameplay
   expansion.

## Foundation completed in this closeout

- Rust 1.98, rustfmt, locked dependency resolution, strict clippy, RustSec, and
  isolated release/DB/canary gates are the reproducible build baseline.
- Persisted staff authority is updated with exact compare-and-swap semantics;
  ambiguous durable outcomes quarantine the player, and direct authenticated
  principal provenance is required at privileged dispatch and publication
  boundaries.
- Password hashing and verification run off the game loop. Creation performs
  one Argon2id computation, targeted password writes commute with ordinary
  saves, and terminal unlock is an asynchronous verified operation.
- OLC writes are fail closed, new-zone multi-file publication has a durable
  crash-recovery marker, and shutdown/copyover remain blocked until unresolved
  publications are retried.
- Mail deletion uses a copy-on-write store replacement, and delayed destructive
  commands revalidate their initiating authenticated session before taking
  effect.

## Issue closure contract

Issue #368 remains the living-world modernization epic. Completed waves stay
recorded, while procedural dungeons and their instance primitives move to a
deferred child epic. The active closeout still requires the QP/economy audit,
negotiated rich client data, a production Rust launch, and a complete scripted
playthrough. No acceptance criterion is silently counted as complete or removed.

Every defect closes only when its reproducer, regression test, relevant full
gate, documentation, and deployment/readback evidence are green. A code change
alone is not closure.
