# ERD-JSON contract (v1)

ERD-JSON is the render contract between the core IR and the canvas: the canvas
never parses DBML/SQL/Prisma, it consumes this. Machine-readable schema:
[`schemas/erdjson.schema.json`](../schemas/erdjson.schema.json) (JSON Schema 2020-12).

```jsonc
{
  "version": 1,
  "nodes": [{
    "id": "users",                 // stable slug of the table name
    "name": "users",
    "position": null,              // {"x":0,"y":0} once a layout ran
    "columns": [{ "name": "id", "type": "int", "isPk": true, "isFk": false,
                  "isUnique": false, "isNullable": false }],
    "note": "optional"
  }],
  "edges": [{
    "id": "posts.user_id->users.id",
    "from": { "table": "posts", "columns": ["user_id"] },   // node ids
    "to":   { "table": "users", "columns": ["id"] },
    "kind": "many-to-one",         // | one-to-one | one-to-many
    "label": "optional",
    "onDelete": "cascade",         // optional: cascade | set null | set default | restrict | no action
    "onUpdate": "no action"        // optional, same values
  }],
  "meta": { "layout": null }       // null | "grid" | "elk" | "dagre" | "manual"
}
```

## Rules

- **Determinism.** Same IR → byte-identical output. No timestamps (`generatedAt`
  is deliberately absent), no random ids. Nodes sorted by `name`, edges by
  `(from, to)`, enums by name; column order is preserved (it is semantic).
  Pretty-printed, 2-space indent, trailing newline.
- **Ids.** Node `id` = lowercase ASCII slug (`[^a-z0-9]` → `_`); collisions get
  `-2`, `-3`, … in name order. Edge `id` = `<from>.<cols joined by +>-><to>.<cols>`,
  collisions suffixed likewise. Edge endpoints reference node **ids**.
- **Flags.** `isFk` is derived from edges (the FK side: `from` for
  many-to-one/one-to-one, `to` for one-to-many) and ignored on parse.
  `isNullable` is `!not_null` as stored in the IR.
- **Extras beyond the minimal contract** (all optional, omitted when empty):
  column `autoincrement`/`default`/`note`, node `indexes[]`, top-level `enums[]`.
  They keep the IR lossless through ERD-JSON; a canvas may ignore them.
- **`default`** is SQL expression text, the same in every format: string
  literals keep their single quotes (`"'user'"`, inner quotes doubled), numbers,
  booleans and `NULL` are bare (`"0"`, `"true"`), functions and other
  expressions are verbatim (`"now()"`, `"'{}'::jsonb"`).
- **`meta.sourceFormat`** is only present when carried through from a parsed
  ERD-JSON document; `emit(ir)` has no knowledge of the original format.
- **`meta.layout`** `grid` is an addition to the original `null|elk|dagre|manual`.

## Layout hook

`erdjson::apply_layout(&mut Value, LayoutStrategy)` fills `position` and
`meta.layout` on an emitted document. `Grid` places node *i* at
`col = i % ceil(sqrt(n))`, `row = i / ceil(sqrt(n))`, `x = col*320`, `y = row*360`
(integers, fixed cell). `elk`/`dagre` join the same enum in R4.

Positions have no IR equivalent: `parse` stores them in the IR `unknown`
passthrough (`erdjson:position {...}` per table, `erdjson:layout <name>` per
schema) and `emit` restores them, so ERD-JSON → IR → ERD-JSON is idempotent.
These `erdjson:*` lines also appear in other codecs' `unknown` passthrough.
