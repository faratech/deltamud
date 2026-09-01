# Security policy

## Reporting a vulnerability

Do not open a public issue for a vulnerability that could expose credentials,
player data, administrative access, or a running server. Use the repository's
private GitHub Security Advisory form instead:

https://github.com/faratech/deltamud/security/advisories/new

Include the affected revision, configuration, reproduction steps, impact, and
any proof-of-concept material that can be shared safely. Never include live
passwords, database URLs, private player communications, or production data.

## Supported code

Security fixes target the current `main` branch and the currently deployed Rust
release. The legacy C server is retained as a compatibility oracle and is not a
supported public production target.

## Security invariants

- The game world has one authoritative owner; edge transports and HTTP helpers
  cannot mutate it directly.
- Production database configuration fails closed and secrets are never logged.
- Legacy password hashes may be verified for migration, but new hashes use the
  current password policy and are upgraded after successful authentication.
- OLC and runtime persistence publish through checked durable replacement; a
  failed save retains both the previous durable file and the pending edit.
- Plain Telnet is a compatibility transport. Production credentials should use
  a reviewed TLS endpoint.
- Copyover accepts only validated snapshots, sockets, and executable paths.

Changes affecting these invariants require focused regression tests plus the
full release gate documented in `rust-mud/docs/RUNBOOK.md`.
