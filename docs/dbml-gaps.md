# DBML gaps

What the dibibest DBML subset does **not** model. Status:

- **unsupported** — the parser returns an error (fails loudly, never silently drops).
- **preserved-as-comment** — parsed into the IR passthrough store (`unknown`) and
  re-emitted as `// unknown: <original>`; round-trips losslessly, but other
  formats (SQL) do not see it.
- **canonicalized** — round-trips (IR-equal, emit-stable) but the text changes.

Corpus: `crates/dibibest-core/tests/corpus/*.dbml`, gate ≥95% in
`tests/corpus_roundtrip.rs`.

## Preserved as comment

| Construct | Example | Notes |
|---|---|---|
| Unknown column settings | `salary numeric [color: red]` | Emitted on the line above the column. |
| Table header settings | `Table t [headercolor: #3498DB] {` | Emitted at the end of the table body. |
| Unknown index settings | `(a) [type: hash]` | Attached to the **table**, not the index; includes `pk` on composite-PK indexes. |
| Ref settings other than `delete` / `update` | `Ref: a.b > c.d [color: #f00]` | Collected in schema-level `unknown`, emitted after the refs (association with the specific Ref is lost). `delete:` / `update:` are modeled as referential actions and map to SQL `ON DELETE` / `ON UPDATE`, Prisma `onDelete` / `onUpdate` and ERD-JSON `onDelete` / `onUpdate`. |
| Explicit `null` | `col int [null]` | Treated as an unknown setting. |
| Project body | `database_type: 'PostgreSQL'`, `Note { … }` | Kept verbatim line-by-line (comments inside are dropped). |
| Enum value settings | `created [note: 'x']` | The whole line is kept as the value text. |

## Canonicalized

| Construct | Example | Becomes |
|---|---|---|
| Setting order | `[note: 'x', pk]` | `[pk, …, note: 'x']` |
| Inline refs | `owner_id int [ref: > users.id]` | Hoisted to a top-level `Ref:` line. |
| Ref blocks | `Ref { a.b > c.d }` | One `Ref:` line per relation. |
| Composite PK | `(a, b) [pk]` in `indexes` | Kept as a normal index plus the `pk` unknown on the table. |
| Comments | `// text` | Dropped (except `// unknown:` lines). |
| Index of one bare column | `name` in `indexes` | `(name)` |

## Unsupported (parse error)

| Construct | Example |
|---|---|
| `TableGroup` | `TableGroup shop { users orders }` |
| Table alias | `Table users as U {` |
| Multi-line notes | `Note: '''line 1`<br>`line 2'''` |
| Table `Note { … }` block | `Note { 'text' }` inside a table |
| Top-level sticky notes | `Note my_note { 'text' }` |
| Many-to-many ref | `Ref: a.id <> b.id` |
| Checks blocks | `checks { `price > 0` }` |
| `TablePartial` / `~partial` injection | `TablePartial base { id int }` |
| Top-level `Records` / data blocks | `Records users(id) { 1 }` |
| Quoted identifiers containing `.` or `"` | `Table "a.b" {` is read as schema `a`, table `b` |

## Fidelity caveats to know

- Index and Ref unknowns (other than referential actions) lose their exact owner (see above); they stay stable
  across round-trips, which is what the corpus gate measures.
- Columns with an unknown setting gain a `// unknown:` line on emit; that line,
  not the original `[…]` entry, is what a second parse reads.
