# SQL dialects (issue #7)

## Decision

One `SqlCodec` implementation, registered three times with a `Dialect` parameter:

| Format id | Dialect |
|---|---|
| `sql` | PostgreSQL (default — existing id is unchanged) |
| `sql:mysql` | MySQL |
| `sql:sqlite` | SQLite |

Registered instances (not suffix-parsing in `Registry::codec`) keep the registry seam
untouched: ids stay plain strings, `ids()` / `dibibest formats` / MCP `list_formats`
list the dialects for free, and no codec imports another.

**Parse** uses sqlparser's `GenericDialect` for all three (it accepts every construct
below). Types are kept as raw strings — `TINYINT(1)` is *not* rewritten to `BOOLEAN`;
fidelity over cleverness. **Emit** is dialect-specific.

## What survives, per dialect

| Feature | `sql` (PG) | `sql:mysql` | `sql:sqlite` |
|---|---|---|---|
| Tables / columns / PK / NOT NULL / UNIQUE / DEFAULT | ✅ | ✅ | ✅ |
| Foreign keys (inline + table-level) | ✅ | ✅ | ✅ |
| Identifier quoting | bare when plain; `"…"` for spaces, dots, reserved words (`user`, `order`), inner `"` doubled | `` `backticks` `` always | same as PostgreSQL |
| Auto-increment (`Column.autoincrement`) | ❌ not emitted (known gap) | ✅ `AUTO_INCREMENT` | ✅ `PRIMARY KEY AUTOINCREMENT` |
| Enums (`CREATE TYPE … AS ENUM`) | ✅ parse + emit | ❌ dropped, reported | ❌ dropped, reported |
| Plain indexes | ✅ | ✅ | ✅ |
| Partial / expression indexes | `caps` ✅ ¹ | ❌ reported dropped | `caps` ✅ ¹ |
| Notes / comments | ❌ reported dropped | ❌ reported dropped | ❌ reported dropped |
| ENGINE / charset / collation | n/a | omitted (minimal subset) | n/a |

¹ Pre-existing emitter limitation: a partial index's `WHERE` predicate is not yet
re-emitted by any dialect (tracked with the COMMENT/CHECK corpus work).

Auto-increment on parse: `AUTO_INCREMENT` (MySQL) and `AUTOINCREMENT` (SQLite) both set
`Column.autoincrement`, whichever codec parses them. SQLite requires the column to be an
inline `INTEGER PRIMARY KEY`; composite PKs with autoincrement are not valid SQLite and
are not special-cased.

## Fidelity honesty

`Caps` is per dialect (`dialects`, `enums`, `expr_indexes`). Converting a schema with
enums to `sql:mysql` / `sql:sqlite` emits no `CREATE TYPE` and the `Fidelity.dropped`
list says `enums have no equivalent in target format`. MySQL's inline `ENUM('a','b')`
column type is a different construct (per-column, unnamed) and is deliberately not
synthesised from named IR enums.

Tests: `crates/dibibest-core/tests/sql_dialects.rs`.
