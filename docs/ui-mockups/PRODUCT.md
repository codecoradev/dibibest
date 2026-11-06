# PRODUCT.md — codename `erdhub` (final name = owner's decision)

## What this product is
Bidirectional database schema tool: **dbdiagram + Liam ERD in one, every format round-trips.**
One schema IR (intermediate representation) in the middle; formats around it:
DBML (dbdiagram) ↔ SQL DDL (PostgreSQL/MySQL/SQLite) ↔ Prisma schema ↔ interactive ERD canvas
↔ live-DB introspect. Main selling point = **round-trip fidelity**: honest two-way conversion,
with a report of what survived and what leaked.

## Who it is for
Developers & data engineers who use dbdiagram.io today (but are uneasy keeping schemas in
someone else's cloud) or Liam ERD (but are frustrated that it is one-way). They need a
self-hosted, fast tool with no account.

## Mode (impeccable)
**Operate** — users complete tasks (import → view → edit → export).
Scanability, consistency, and dev-tool expectations (VS Code/dbdiagram-like) win over expression.
The brand lives in precision of detail, not decoration.

## Platform & stack (owner mandate, non-negotiable)
- Core codec: pure Rust, in-process, compilable to WASM + CLI.
- UI: Svelte 5 + svelte-flow (canvas), self-hosted web.
- Metadata: SQLite. Monorepo, default branch `develop`.

## Explicit brief from owner (22 Oct 2026, Discord #research)
- "dbdiagram + Liam, and it should round-trip too" — round-trip is the core feature, not a bonus.
- REA (morluto/rea) = research tool ONLY, not a product component.
- Rust + Svelte stack "as usual".

## ASSUMPTIONS (labeled — not yet confirmed by owner)
- A1: Web-first desktop-class app (not native desktop) — inferred from the dbdiagram/Liam comparison.
- A2: Single-user local first; real-time collaboration = v1 non-goal — inferred from complexity.
- A3: Open-source MIT/Apache — inferred from the fleet's product pattern (not yet decided by owner).
- A4: Visual: dark-first tool aesthetic like modern dev tools (VS Code/Liam vibes) — no official
  brand direction yet; mockup v1 uses dark, evaluated in critique.
