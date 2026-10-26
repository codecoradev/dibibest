//! ERD-JSON: the render contract between the core IR and the canvas.
//! Contract: `docs/erdjson-schema.md` + `schemas/erdjson.schema.json`.
//!
//! Emission is deterministic (no timestamps/random ids; nodes sorted by name,
//! edges by (from,to)). Layout data has no IR equivalent, so `parse` stashes it
//! in the IR `unknown` passthrough (`erdjson:*` lines) and `emit` restores it —
//! that is what keeps ERD-JSON → IR → ERD-JSON byte-identical.

use crate::ir::{Column, EnumDef, IndexDef, Ref, RefKind, SchemaIR, Table};
use crate::{Caps, Codec, CoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};
use std::collections::{BTreeMap, HashMap};

const FMT: &str = "erdjson";
const VERSION: u32 = 1;
const POS: &str = "erdjson:position ";
const LAYOUT: &str = "erdjson:layout ";
const SOURCE: &str = "erdjson:sourceFormat ";

pub struct ErdJsonCodec;

#[derive(Serialize, Deserialize)]
struct Doc {
    version: u32,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    enums: Vec<EnumDef>,
    meta: Meta,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Meta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_format: Option<String>,
    layout: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Node {
    id: String,
    name: String,
    position: Option<Pos>,
    columns: Vec<Col>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    indexes: Vec<IndexDef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Pos {
    x: Number,
    y: Number,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Col {
    name: String,
    #[serde(rename = "type")]
    ty: String,
    is_pk: bool,
    is_fk: bool,
    is_unique: bool,
    is_nullable: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    autoincrement: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Serialize, Deserialize)]
struct Edge {
    id: String,
    from: End,
    to: End,
    kind: RefKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct End {
    table: String,
    columns: Vec<String>,
}

/// Stable slug of a table name: lowercase ASCII alphanumerics, `_` elsewhere.
fn slug(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Make `base` unique within `seen` by suffixing `-2`, `-3`, … on collision.
fn unique(seen: &mut HashMap<String, usize>, base: String) -> String {
    let n = seen.entry(base.clone()).or_insert(0);
    *n += 1;
    if *n == 1 {
        base
    } else {
        format!("{base}-{n}")
    }
}

fn stash<'a>(lines: &'a [String], prefix: &str) -> Option<&'a str> {
    lines.iter().find_map(|l| l.strip_prefix(prefix))
}

fn err(message: impl Into<String>) -> CoreError {
    CoreError::Parse {
        format: FMT,
        message: message.into(),
    }
}

impl Codec for ErdJsonCodec {
    fn scan(&self, src: &str) -> Option<crate::SourceCounts> {
        crate::scan::erdjson(src)
    }

    fn id(&self) -> &str {
        FMT
    }

    fn caps(&self) -> Caps {
        Caps {
            indexes: true,
            expr_indexes: true,
            enums: true,
            defaults: true,
            notes: true,
            dialects: &[],
        }
    }

    fn emit(&self, ir: &SchemaIR) -> Result<String, CoreError> {
        let mut tables: Vec<&Table> = ir.tables.iter().collect();
        tables.sort_by(|a, b| a.name.cmp(&b.name));
        let mut seen = HashMap::new();
        let ids: BTreeMap<&str, String> = tables
            .iter()
            .map(|t| (t.name.as_str(), unique(&mut seen, slug(&t.name))))
            .collect();
        let id_of = |t: &str| ids.get(t).cloned().unwrap_or_else(|| slug(t));

        // FK side: many-to-one / one-to-one → `from`; one-to-many → `to`.
        let fk = |table: &str, col: &str| {
            ir.refs.iter().any(|r| {
                let (t, cols) = match r.kind {
                    RefKind::OneToMany => (&r.to_table, &r.to_cols),
                    _ => (&r.from_table, &r.from_cols),
                };
                t == table && cols.iter().any(|c| c == col)
            })
        };

        let nodes = tables
            .iter()
            .map(|t| Node {
                id: id_of(&t.name),
                name: t.name.clone(),
                position: stash(&t.unknown, POS).and_then(|p| serde_json::from_str(p).ok()),
                columns: t
                    .columns
                    .iter()
                    .map(|c| Col {
                        name: c.name.clone(),
                        ty: c.ty.clone(),
                        is_pk: c.pk,
                        is_fk: fk(&t.name, &c.name),
                        is_unique: c.unique,
                        is_nullable: !c.not_null,
                        autoincrement: c.autoincrement,
                        default: c.default.clone(),
                        note: c.note.clone(),
                    })
                    .collect(),
                indexes: t.indexes.clone(),
                note: t.note.clone(),
            })
            .collect();

        let mut edges: Vec<Edge> = ir
            .refs
            .iter()
            .map(|r| Edge {
                id: String::new(),
                from: End {
                    table: id_of(&r.from_table),
                    columns: r.from_cols.clone(),
                },
                to: End {
                    table: id_of(&r.to_table),
                    columns: r.to_cols.clone(),
                },
                kind: r.kind,
                label: r.name.clone(),
            })
            .collect();
        let key = |e: &Edge| {
            format!(
                "{}.{}->{}.{}",
                e.from.table,
                e.from.columns.join("+"),
                e.to.table,
                e.to.columns.join("+")
            )
        };
        edges.sort_by_cached_key(|e| (key(e), e.label.clone()));
        let mut seen = HashMap::new();
        for e in &mut edges {
            e.id = unique(&mut seen, key(e));
        }

        let mut enums = ir.enums.clone();
        enums.sort_by(|a, b| a.name.cmp(&b.name));

        let doc = Doc {
            version: VERSION,
            nodes,
            edges,
            enums,
            meta: Meta {
                source_format: stash(&ir.unknown, SOURCE).map(str::to_string),
                layout: stash(&ir.unknown, LAYOUT).map(str::to_string),
            },
        };
        let mut out = serde_json::to_string_pretty(&doc)
            .map_err(|e| CoreError::Internal(format!("erdjson serialize: {e}")))?;
        out.push('\n');
        Ok(out)
    }

    fn parse(&self, src: &str) -> Result<SchemaIR, CoreError> {
        let doc: Doc = serde_json::from_str(src).map_err(|e| err(e.to_string()))?;
        if doc.version != VERSION {
            return Err(err(format!("unsupported version {}", doc.version)));
        }
        let names: HashMap<&str, &str> = doc
            .nodes
            .iter()
            .map(|n| (n.id.as_str(), n.name.as_str()))
            .collect();
        let name_of = |id: &str| {
            names
                .get(id)
                .map(|n| n.to_string())
                .ok_or_else(|| err(format!("edge references unknown node id `{id}`")))
        };

        let mut ir = SchemaIR::default();
        for n in &doc.nodes {
            let mut unknown = Vec::new();
            if let Some(p) = &n.position {
                let p = serde_json::to_string(p).map_err(|e| err(e.to_string()))?;
                unknown.push(format!("{POS}{p}"));
            }
            ir.tables.push(Table {
                name: n.name.clone(),
                columns: n
                    .columns
                    .iter()
                    .map(|c| Column {
                        name: c.name.clone(),
                        ty: c.ty.clone(),
                        pk: c.is_pk,
                        not_null: !c.is_nullable,
                        unique: c.is_unique,
                        autoincrement: c.autoincrement,
                        default: c.default.clone(),
                        note: c.note.clone(),
                        unknown: Vec::new(),
                    })
                    .collect(),
                indexes: n.indexes.clone(),
                note: n.note.clone(),
                unknown,
            });
        }
        for e in &doc.edges {
            ir.refs.push(Ref {
                from_table: name_of(&e.from.table)?,
                from_cols: e.from.columns.clone(),
                to_table: name_of(&e.to.table)?,
                to_cols: e.to.columns.clone(),
                kind: e.kind,
                name: e.label.clone(),
            });
        }
        ir.enums = doc.enums;
        if let Some(l) = &doc.meta.layout {
            ir.unknown.push(format!("{LAYOUT}{l}"));
        }
        if let Some(s) = &doc.meta.source_format {
            ir.unknown.push(format!("{SOURCE}{s}"));
        }
        Ok(ir)
    }
}

/// Layout strategies. elk/dagre arrive with R4 behind this same enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutStrategy {
    /// Row-major grid in node order, fixed cell size.
    Grid,
}

pub const GRID_CELL_W: i64 = 320;
pub const GRID_CELL_H: i64 = 360;

/// Fill `nodes[].position` and `meta.layout` of an ERD-JSON document in place.
/// Deterministic: same document + strategy → same positions.
pub fn apply_layout(doc: &mut Value, strategy: LayoutStrategy) -> Result<(), CoreError> {
    let nodes = doc
        .get_mut("nodes")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| err("document has no `nodes` array"))?;
    let (n, label) = match strategy {
        LayoutStrategy::Grid => (nodes.len(), "grid"),
    };
    // ponytail: fixed cell ignores node height; size-aware cells land with elk/dagre (R4).
    let cols = ((n as f64).sqrt().ceil() as usize).max(1);
    for (i, node) in nodes.iter_mut().enumerate() {
        let (row, col) = ((i / cols) as i64, (i % cols) as i64);
        node["position"] = serde_json::json!({"x": col * GRID_CELL_W, "y": row * GRID_CELL_H});
    }
    doc["meta"]["layout"] = Value::String(label.into());
    Ok(())
}
