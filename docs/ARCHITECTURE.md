# Architecture

The hybrid-registry design (full rationale: design-twice comparison, 22 Oct 2026 —
3 interface variants evaluated on depth/leverage/locality; recommendation: registry
inside, two-function API outside).

```
                 ┌── Codec: DbmlCodec        (parse/emit DBML)
  SchemaIR ◄────►├── Codec: SqlCodec         (parse/emit SQL DDL via sqlparser-rs)
    (hub)        ├── Codec: PrismaCodec      (R4 — prisma-engines is Rust)
                 └── Codec: ErdJsonCodec     (R3 — canvas contract)

  Registry (owns codecs)  ── public API ──►  convert(src, to)
                                          convert_explicit(src, from, to)
                                          → Conversion { output, Fidelity }
```

## Decisions

1. **IR hub, not pairwise converters.** N formats = 2N codecs, not N² paths.
   Adding a format never touches another one (kill-test: Drizzle = 1 file +
   1 register line, zero edits elsewhere).
2. **Fidelity is measured, not claimed.** `convert_explicit` re-parses its own
   output and diffs counts against the source IR (`tables/columns/refs/indexes
   kept/total`), plus an idempotency check in the source format
   (`parse(emit(parse(src))) == parse(src)`). Both ship in `Conversion` and are
   tested in CI.
3. **Core is pure.** No I/O in `dibibest-core` — it compiles to WASM (R3 canvas)
   and links into the CLI and MCP server unchanged.
4. **Codec trait = the seam.** Two real adapters exist per format slot today
   (production codec + conformance fixtures); community codecs plug in without
   core changes later.
5. **MCP is a thin layer** over the same core; no second conversion
   implementation exists anywhere in the repo.

6. **ERD-JSON is the canvas contract.** The `erdjson` codec emits deterministic
   render data (nodes/edges/meta, see `docs/erdjson-schema.md`); layout runs as a
   separate step (`erdjson::apply_layout`) so snapshots stay stable.

## Known scaffold limitations (tracked)

- DBML subset (Project/Table/Ref/Enum/indexes+expressions/settings). Unknown
  settings are ignored on parse today — corpus conformance (R1) will switch this
  to preserve-as-comment so round-trips stay lossless on unknowns.
- SQL emit is PG-flavored; dialect selection arrives with R2.
- ObjectName quoting is minimal; identifier edge cases belong to the R1 corpus.

## Storage (`dibibest-store`, epic #84)

Projects live on the server (decision of 2026-11-24: own VPS, one origin, SQLite). The crate is the seam:

- `Store`: projects, an append-only **op log** with one total order per project (`seq` from 1), and **snapshots**.
  The store never interprets an op; the payload is opaque JSON text and every row carries a `schema_version`.
- An op carries an **idempotency key**, unique per project. A retry is answered with the same `seq` and writes
  nothing; a key reused by another actor or for other content is refused.
- Adapters: `MemoryStore` (reference and test double) and `SqliteStore`. Both run the same contract cases
  (`store_contract!`), so a Postgres adapter can be added without touching the room or the server.

`SqliteStore` rules, each covered by a test:

1. **One writer thread, a bounded queue.** SQLite has one writer; callers that race for the lock get `SQLITE_BUSY`.
   A full queue answers `Busy` at once and writes nothing.
2. **Group commit.** Waiting jobs share one transaction (a savepoint per job). A caller is answered after `COMMIT`,
   so an acknowledged op is durable, and nothing may be broadcast before that.
3. **WAL, checked.** `journal_mode=WAL` is requested and the answer is verified; the open fails otherwise.
4. **`synchronous=FULL`.** `NORMAL` can lose a commit that clients already saw, and its `seq` would be reused.
5. **Readers** are read-only connections; replay is read page by page so no read holds back the WAL checkpoint.
6. **Disk full** turns the store read-only (`read_only()`, to be shown on `/readyz`); it clears when a write succeeds.
7. **Backup** is `backup_to` (`VACUUM INTO`), never a copy of the `.db` file: what is still in `-wal` would be missed.

Operating rules: exactly one server process per database (the room state is in memory; the lock file comes with the
server crate, #91); keep the data directory on a local disk or a Docker **named volume**, not a Docker Desktop bind
mount (WAL on VirtioFS lost data in another project of ours).
