//! Prisma schema codec — hand-written parser + emitter (see
//! `docs/prisma-integration.md` for why prisma-engines is not a dependency).
//!
//! Subset: models, scalar/enum/`Unsupported` fields, `@id @unique @default
//! @map @db.*`, `@@id @@unique @@index @@map`, `@relation` (1-n / 1-1, named),
//! enums, `///` doc comments as notes. Everything the IR carries but Prisma
//! cannot express is kept in `// unknown:` / `// project:` comments so a
//! Prisma → IR → Prisma trip stays lossless; expression/partial indexes are
//! dropped (and reported by the registry via `Caps::expr_indexes = false`).

use crate::ir::*;
use crate::{Caps, Codec, CoreError};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

pub struct PrismaCodec;

const FMT: &str = "prisma";
const UNKNOWN: &str = "// unknown:";
const PROJECT: &str = "// project:";
const PROJECT_BODY: &str = "// project-body:";

impl Codec for PrismaCodec {
    fn scan(&self, src: &str) -> Option<crate::SourceCounts> {
        Some(crate::scan::prisma(src))
    }

    fn id(&self) -> &str {
        "prisma"
    }

    fn caps(&self) -> Caps {
        Caps {
            indexes: true,
            expr_indexes: false,
            enums: true,
            defaults: true,
            notes: true,
            dialects: &[],
        }
    }

    fn parse(&self, src: &str) -> Result<SchemaIR, CoreError> {
        parse_prisma(src)
    }

    fn emit(&self, ir: &SchemaIR) -> Result<String, CoreError> {
        Ok(emit_prisma(ir))
    }
}

// ── Lexical helpers ─────────────────────────────────────────────────────────

/// Byte offsets + chars of `s` outside `"…"` strings (backslash escapes).
fn outside_str(s: &str) -> impl Iterator<Item = (usize, char)> + '_ {
    let (mut inq, mut esc) = (false, false);
    s.char_indices().filter(move |&(_, c)| {
        if esc {
            esc = false;
            return false;
        }
        if inq {
            match c {
                '\\' => esc = true,
                '"' => inq = false,
                _ => {}
            }
            return false;
        }
        if c == '"' {
            inq = true;
            return false;
        }
        true
    })
}

/// Split on `sep` at bracket depth 0, outside strings; empty parts dropped.
fn split_top(s: &str, sep: char) -> Vec<String> {
    let (mut out, mut start, mut depth) = (Vec::new(), 0, 0i32);
    for (i, c) in outside_str(s) {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(s[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].trim().to_string());
    out.retain(|x| !x.is_empty());
    out
}

fn strip_line_comment(l: &str) -> &str {
    let cs: Vec<_> = outside_str(l).collect();
    match cs
        .windows(2)
        .find(|w| w[0].1 == '/' && w[1].1 == '/' && w[1].0 == w[0].0 + 1)
    {
        Some(w) => l[..w[0].0].trim(),
        None => l,
    }
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let Some(inner) = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')) else {
        return s.to_string();
    };
    let mut out = String::new();
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        out.push(if c == '\\' {
            it.next().unwrap_or('\\')
        } else {
            c
        });
    }
    out
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `(@name, args)` pairs of an attribute tail like `@id @default(now()) @db.Text`.
fn attrs(tail: &str) -> Vec<(String, String)> {
    let cs: Vec<(usize, char)> = outside_str(tail).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        if cs[i].1 != '@' {
            i += 1;
            continue;
        }
        let start = cs[i].0;
        while i < cs.len()
            && (cs[i].1 == '@' || cs[i].1.is_alphanumeric() || "_.".contains(cs[i].1))
        {
            i += 1;
        }
        let name_end = cs.get(i).map_or(tail.len(), |c| c.0);
        let name = tail[start..name_end].to_string();
        let mut args = String::new();
        if cs.get(i).is_some_and(|c| c.1 == '(') {
            // balanced scan over the raw text so strings inside args survive
            let (mut depth, mut inq, mut esc) = (0, false, false);
            for (j, c) in tail[cs[i].0..].char_indices() {
                if esc {
                    esc = false;
                } else if inq {
                    match c {
                        '\\' => esc = true,
                        '"' => inq = false,
                        _ => {}
                    }
                } else if c == '"' {
                    inq = true;
                } else if c == '(' {
                    depth += 1;
                } else if c == ')' {
                    depth -= 1;
                    if depth == 0 {
                        args = tail[cs[i].0 + 1..cs[i].0 + j].to_string();
                        let end = cs[i].0 + j;
                        while i < cs.len() && cs[i].0 <= end {
                            i += 1;
                        }
                        break;
                    }
                }
            }
        }
        out.push((name, args));
    }
    out
}

fn attr<'a>(a: &'a [(String, String)], name: &str) -> Option<&'a str> {
    a.iter().find(|x| x.0 == name).map(|x| x.1.as_str())
}

/// Value of `key: value` among comma-separated args.
fn named(args: &str, key: &str) -> Option<String> {
    split_top(args, ',').into_iter().find_map(|p| {
        let (k, v) = p.split_once(':')?;
        (k.trim() == key).then(|| v.trim().to_string())
    })
}

/// First positional (non `key:`) arg.
fn positional(args: &str) -> Option<String> {
    split_top(args, ',')
        .into_iter()
        .find(|p| !p.split_once(':').is_some_and(|(k, _)| is_ident(k.trim())))
}

fn list(v: &str) -> Vec<String> {
    let v = v.trim();
    let inner = v
        .strip_prefix('[')
        .and_then(|x| x.strip_suffix(']'))
        .unwrap_or(v);
    split_top(inner, ',')
}

fn is_ident(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Prisma identifier for an arbitrary IR name (original kept via `@map`).
fn sanitize(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    match t.chars().next() {
        None => "unnamed".into(),
        Some(c) if c.is_ascii_alphabetic() => t,
        _ => format!("n{t}"),
    }
}

// ── Type & default mapping ──────────────────────────────────────────────────

/// IR type → (Prisma scalar, optional `@db.X` attribute body, forced autoincrement).
fn ty_to_prisma(ty: &str) -> Option<(&'static str, Option<String>, bool)> {
    let t = ty.trim().to_ascii_lowercase();
    let (base, params) = match t.split_once('(') {
        Some((b, p)) => (b.trim(), Some(p.trim_end_matches(')').trim().to_string())),
        None => (t.as_str(), None),
    };
    let db = |n: &str| {
        Some(match &params {
            Some(p) => format!("{n}({})", split_top(p, ',').join(", ")),
            None => n.to_string(),
        })
    };
    Some(match base {
        "int" | "integer" | "int4" => ("Int", None, false),
        "smallint" | "int2" => ("Int", db("SmallInt"), false),
        "bigint" | "int8" => ("BigInt", None, false),
        "serial" => ("Int", None, true),
        "bigserial" => ("BigInt", None, true),
        "varchar" | "character varying" => ("String", db("VarChar"), false),
        "char" | "character" => ("String", db("Char"), false),
        "text" => ("String", db("Text"), false),
        "uuid" => ("String", db("Uuid"), false),
        "boolean" | "bool" => ("Boolean", None, false),
        "float" | "float8" => ("Float", None, false),
        "double" | "double precision" => ("Float", db("DoublePrecision"), false),
        "real" | "float4" => ("Float", db("Real"), false),
        "numeric" | "decimal" => ("Decimal", params.as_ref().and(db("Decimal")), false),
        "timestamp" | "datetime" => ("DateTime", None, false),
        "timestamptz" => ("DateTime", db("Timestamptz"), false),
        "date" => ("DateTime", db("Date"), false),
        "time" => ("DateTime", db("Time"), false),
        "json" => ("Json", db("Json"), false),
        "jsonb" => ("Json", None, false),
        "bytea" => ("Bytes", None, false),
        _ => return None,
    })
}

/// Prisma scalar (+ `@db.X(args)`) → IR type.
fn ty_from_prisma(scalar: &str, db: Option<&str>) -> String {
    if let Some(d) = db {
        let d = d.strip_prefix("db.").unwrap_or(d);
        let (n, args) = d.split_once('(').unwrap_or((d, ""));
        let args = split_top(args.trim_end_matches(')'), ',').join(", ");
        let n = match n.to_ascii_lowercase().as_str() {
            "decimal" => "numeric".into(),
            "doubleprecision" => "double precision".into(),
            n => n.to_string(),
        };
        return if args.is_empty() {
            n
        } else {
            format!("{n}({args})")
        };
    }
    match scalar {
        "Int" => "integer",
        "BigInt" => "bigint",
        "String" => "varchar",
        "Boolean" => "boolean",
        "Float" => "float",
        "Decimal" => "numeric",
        "DateTime" => "timestamp",
        "Json" => "jsonb",
        "Bytes" => "bytea",
        other => other,
    }
    .to_string()
}

const NOW: [&str; 2] = ["now()", "current_timestamp"];
const PRISMA_FNS: [&str; 4] = ["uuid()", "cuid()", "now()", "nanoid()"];

fn default_to_prisma(d: &str, string_col: bool) -> String {
    let l = d.to_ascii_lowercase();
    if NOW.contains(&l.as_str()) {
        "now()".into()
    } else if PRISMA_FNS.contains(&d) {
        d.into()
    } else if l == "true" || l == "false" || d.parse::<f64>().is_ok() {
        l
    } else if string_col && !d.contains('(') {
        quote(d)
    } else {
        format!("dbgenerated({})", quote(d))
    }
}

fn default_from_prisma(args: &str) -> Option<String> {
    let a = args.trim();
    if let Some(inner) = a
        .strip_prefix("dbgenerated(")
        .and_then(|x| x.strip_suffix(')'))
    {
        return Some(unquote(inner));
    }
    Some(if a.starts_with('"') {
        unquote(a)
    } else {
        a.to_string()
    })
}

// ── Parse ───────────────────────────────────────────────────────────────────

struct RawField {
    name: String,
    base: String,
    optional: bool,
    list: bool,
    attrs: Vec<(String, String)>,
    note: Option<String>,
    unknown: Vec<String>,
}

struct RawModel {
    name: String,
    fields: Vec<RawField>,
    attrs: Vec<(String, String)>,
    note: Option<String>,
    unknown: Vec<String>,
}

enum Block {
    Model(RawModel),
    Enum(EnumDef, String),
    Skip,
}

fn perr(line: usize, m: &str) -> CoreError {
    CoreError::Parse {
        format: FMT,
        message: format!("line {}: {}", line + 1, m),
    }
}

/// `Type`, `Type?`, `Type[]`, `Unsupported("x")?` → (base, optional, list).
fn split_type(t: &str) -> (String, bool, bool) {
    if let Some(b) = t.strip_suffix("[]") {
        (b.into(), false, true)
    } else if let Some(b) = t.strip_suffix('?') {
        (b.into(), true, false)
    } else {
        (t.into(), false, false)
    }
}

fn parse_field(l: &str, line: usize) -> Result<RawField, CoreError> {
    let l = strip_line_comment(l);
    let (name, rest) = l
        .split_once(char::is_whitespace)
        .ok_or_else(|| perr(line, "field needs a type"))?;
    let rest = rest.trim_start();
    // the type token ends at the first space outside parens/strings
    let mut depth = 0;
    let end = outside_str(rest)
        .find_map(|(i, c)| match c {
            '(' => {
                depth += 1;
                None
            }
            ')' => {
                depth -= 1;
                None
            }
            c if c.is_whitespace() && depth == 0 => Some(i),
            _ => None,
        })
        .unwrap_or(rest.len());
    let (base, optional, list) = split_type(&rest[..end]);
    Ok(RawField {
        name: name.to_string(),
        base,
        optional,
        list,
        attrs: attrs(&rest[end..]),
        note: None,
        unknown: Vec::new(),
    })
}

fn doc_note(doc: &mut Vec<String>) -> Option<String> {
    (!doc.is_empty()).then(|| std::mem::take(doc).join("\n"))
}

fn parse_prisma(src: &str) -> Result<SchemaIR, CoreError> {
    let mut ir = SchemaIR::default();
    let mut models: Vec<RawModel> = Vec::new();
    let mut enum_maps: Vec<(String, String)> = Vec::new(); // sanitized → original
    let mut block: Option<Block> = None;
    let (mut doc, mut pending): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());

    for (n, raw) in src.lines().enumerate() {
        let line = raw.trim();
        if let Some(d) = line.strip_prefix("///") {
            doc.push(d.strip_prefix(' ').unwrap_or(d).to_string());
            continue;
        }
        if let Some(u) = line.strip_prefix(UNKNOWN) {
            match &block {
                None => ir.unknown.push(u.trim().to_string()),
                _ => pending.push(u.trim().to_string()),
            }
            continue;
        }
        if block.is_none() {
            if let Some(p) = line.strip_prefix(PROJECT) {
                ir.project = Some(p.trim().to_string());
                continue;
            }
            if let Some(p) = line.strip_prefix(PROJECT_BODY) {
                ir.project_body
                    .push(p.strip_prefix(' ').unwrap_or(p).to_string());
                continue;
            }
        }
        // enum values keep their trailing `// dbml-settings` comment, so only
        // strip comments elsewhere
        let in_enum = matches!(block, Some(Block::Enum(..)));
        let code = if in_enum {
            line
        } else {
            strip_line_comment(line)
        };
        if code.is_empty() || code.starts_with("//") {
            continue;
        }
        match (&mut block, code) {
            (Some(b), "}") => {
                match b {
                    Block::Model(m) => {
                        m.unknown.append(&mut pending);
                        models.push(std::mem::replace(m, empty_model()));
                    }
                    Block::Enum(e, pname) => {
                        enum_maps.push((pname.clone(), e.name.clone()));
                        ir.enums.push(std::mem::take(e));
                    }
                    Block::Skip => {}
                }
                pending.clear();
                doc.clear();
                block = None;
            }
            (Some(Block::Model(m)), l) if l.starts_with("@@") => {
                let a = attrs(l);
                if a.is_empty() {
                    return Err(perr(n, "bad block attribute"));
                }
                m.attrs.push(a[0].clone());
            }
            (Some(Block::Model(m)), l) => {
                let mut f = parse_field(l, n)?;
                f.note = doc_note(&mut doc);
                f.unknown = std::mem::take(&mut pending);
                m.fields.push(f);
            }
            (Some(Block::Enum(e, _)), l) if l.starts_with("@@map") => {
                e.name = attrs(l).first().map(|a| unquote(&a.1)).unwrap_or_default();
            }
            (Some(Block::Enum(e, _)), l) => {
                let (head, comment) = l.split_once("//").unwrap_or((l, ""));
                let a = attrs(head);
                let tok = head.split_whitespace().next().unwrap_or_default();
                let mapped = attr(&a, "@map").map(unquote);
                let mut v = mapped.unwrap_or_else(|| tok.to_string());
                if !comment.trim().is_empty() {
                    v = format!("{v} {}", comment.trim());
                }
                e.values.push(v);
            }
            (Some(Block::Skip), _) => {}
            (None, l) => {
                let Some(head) = l.strip_suffix('{') else {
                    return Err(perr(
                        n,
                        "expected a block (`model X {`) — one-line blocks are unsupported",
                    ));
                };
                let parts: Vec<_> = head.split_whitespace().collect();
                match parts.as_slice() {
                    ["model", name] => {
                        block = Some(Block::Model(RawModel {
                            name: name.to_string(),
                            note: doc_note(&mut doc),
                            ..empty_model()
                        }));
                    }
                    ["enum", name] => {
                        block = Some(Block::Enum(
                            EnumDef {
                                name: name.to_string(),
                                values: Vec::new(),
                            },
                            name.to_string(),
                        ));
                    }
                    ["datasource" | "generator" | "type" | "view", _] => block = Some(Block::Skip),
                    _ => return Err(perr(n, "unsupported top-level block")),
                }
                doc.clear();
            }
        }
    }
    if block.is_some() {
        return Err(CoreError::Parse {
            format: FMT,
            message: "unexpected end of input: block not closed".into(),
        });
    }
    resolve(&mut ir, models, &enum_maps)?;
    Ok(ir)
}

fn empty_model() -> RawModel {
    RawModel {
        name: String::new(),
        fields: Vec::new(),
        attrs: Vec::new(),
        note: None,
        unknown: Vec::new(),
    }
}

/// Auto-generated relation name (emitted only to disambiguate; stripped on parse
/// so it never leaks into the IR as a user-chosen name).
fn auto_rel_name(fk_model: &str, fk_fields: &[String]) -> String {
    format!("{fk_model}_{}", fk_fields.join("_"))
}

fn resolve(
    ir: &mut SchemaIR,
    models: Vec<RawModel>,
    enum_maps: &[(String, String)],
) -> Result<(), CoreError> {
    let enum_of: HashMap<&str, &str> = enum_maps
        .iter()
        .map(|e| (e.0.as_str(), e.1.as_str()))
        .collect();
    // Prisma model name → table name
    let table_of: HashMap<String, String> = models
        .iter()
        .map(|m| {
            let t = attr(&m.attrs, "@@map").map_or(m.name.clone(), unquote);
            (m.name.clone(), t)
        })
        .collect();

    for m in &models {
        // Prisma field name → column name (scalar fields only)
        let col_of: HashMap<&str, String> = m
            .fields
            .iter()
            .filter(|f| !table_of.contains_key(&f.base))
            .map(|f| {
                (
                    f.name.as_str(),
                    attr(&f.attrs, "@map").map_or(f.name.clone(), unquote),
                )
            })
            .collect();
        let cols = |names: Vec<String>| -> Vec<String> {
            names
                .into_iter()
                .map(|n| col_of.get(n.as_str()).cloned().unwrap_or(n))
                .collect()
        };
        let composite_pk: Vec<String> = attr(&m.attrs, "@@id")
            .map(|a| list(&positional_list(a)))
            .unwrap_or_default();
        let composite_pk = cols(composite_pk);
        let mut t = Table {
            name: table_of[&m.name].clone(),
            note: m.note.clone(),
            unknown: m.unknown.clone(),
            ..Table::default()
        };
        let mut unique_sets: Vec<Vec<String>> = Vec::new();
        for (name, args) in &m.attrs {
            let is_unique = name == "@@unique";
            match name.as_str() {
                "@@id" | "@@map" => {}
                "@@unique" | "@@index" => {
                    let columns = cols(list(&positional_list(args)));
                    if is_unique {
                        unique_sets.push(columns.clone());
                    }
                    t.indexes.push(IndexDef {
                        name: named(args, "map")
                            .or_else(|| named(args, "name"))
                            .map(|s| unquote(&s)),
                        columns,
                        unique: is_unique,
                        expr: None,
                    });
                }
                other => t.unknown.push(format!("{other}({args})")),
            }
        }
        for f in &m.fields {
            if table_of.contains_key(&f.base) {
                continue; // relation field: handled below; back-references carry no data
            }
            let colname = col_of[f.name.as_str()].clone();
            let db = f.attrs.iter().find(|a| a.0.starts_with("@db.")).map(|a| {
                if a.1.is_empty() {
                    a.0.clone()
                } else {
                    format!("{}({})", a.0, a.1)
                }
            });
            let mut ty = if let Some(u) = f.base.strip_prefix("Unsupported(") {
                unquote(u.trim_end_matches(')'))
            } else if let Some(orig) = enum_of.get(f.base.as_str()) {
                orig.to_string()
            } else {
                ty_from_prisma(&f.base, db.as_deref().map(|d| d.trim_start_matches('@')))
            };
            if f.list {
                ty.push_str("[]");
            }
            let pk = attr(&f.attrs, "@id").is_some() || composite_pk.contains(&colname);
            let default = attr(&f.attrs, "@default");
            let autoinc = default.is_some_and(|d| d.trim() == "autoincrement()");
            t.columns.push(Column {
                name: colname,
                ty,
                pk,
                not_null: pk || !(f.optional || f.list),
                unique: attr(&f.attrs, "@unique").is_some(),
                autoincrement: autoinc,
                default: default.filter(|_| !autoinc).and_then(default_from_prisma),
                note: f.note.clone(),
                unknown: f.unknown.clone(),
            });
            if attr(&f.attrs, "@unique").is_some() {
                unique_sets.push(vec![t.columns.last().unwrap().name.clone()]);
            }
            if pk && composite_pk.is_empty() {
                unique_sets.push(vec![t.columns.last().unwrap().name.clone()]);
            }
        }
        if !composite_pk.is_empty() {
            unique_sets.push(composite_pk);
        }

        for f in &m.fields {
            let Some(target) = table_of.get(&f.base) else {
                continue;
            };
            let Some(rel) = attr(&f.attrs, "@relation") else {
                continue;
            };
            let (Some(fk), Some(refs)) = (named(rel, "fields"), named(rel, "references")) else {
                continue; // the other side of a relation
            };
            let fk_fields = list(&fk);
            let fk_cols = cols(fk_fields.clone());
            let target_model = &f.base;
            let target_cols: Vec<String> = {
                let tm = models.iter().find(|x| &x.name == target_model);
                list(&refs)
                    .into_iter()
                    .map(|n| {
                        tm.and_then(|tm| tm.fields.iter().find(|ff| ff.name == n))
                            .and_then(|ff| attr(&ff.attrs, "@map").map(unquote))
                            .unwrap_or(n)
                    })
                    .collect()
            };
            let one = unique_sets
                .iter()
                .any(|u| u.len() == fk_cols.len() && fk_cols.iter().all(|c| u.contains(c)));
            let name = positional(rel)
                .or_else(|| named(rel, "name"))
                .map(|s| unquote(&s))
                .filter(|n| *n != auto_rel_name(&m.name, &fk_fields));
            ir.refs.push(Ref {
                from_table: t.name.clone(),
                from_cols: fk_cols,
                to_table: target.clone(),
                to_cols: target_cols,
                kind: if one {
                    RefKind::OneToOne
                } else {
                    RefKind::ManyToOne
                },
                name,
            });
        }
        ir.tables.push(t);
    }
    Ok(())
}

/// First positional argument of `@@x([a, b], map: "n")`, i.e. the list.
fn positional_list(args: &str) -> String {
    named(args, "fields")
        .or_else(|| positional(args))
        .unwrap_or_default()
}

// ── Emit ────────────────────────────────────────────────────────────────────

/// Name `s` as a unique identifier within `used`.
fn unique_name(base: String, used: &mut HashSet<String>) -> String {
    let mut n = base.clone();
    let mut i = 1;
    while !used.insert(n.clone()) {
        i += 1;
        n = format!("{base}_{i}");
    }
    n
}

fn doc_lines(out: &mut String, indent: &str, note: &str) {
    for l in note.split('\n') {
        let _ = writeln!(out, "{indent}/// {l}");
    }
}

fn emit_prisma(ir: &SchemaIR) -> String {
    let mut out = String::new();
    if let Some(p) = &ir.project {
        let _ = writeln!(out, "{PROJECT} {p}");
    }
    for l in &ir.project_body {
        let _ = writeln!(out, "{PROJECT_BODY} {l}");
    }
    let _ = writeln!(
        out,
        "datasource db {{\n  provider = \"postgresql\"\n  url      = env(\"DATABASE_URL\")\n}}\n"
    );

    let enum_name = |ty: &str| {
        let b = ty.strip_suffix("[]").unwrap_or(ty);
        ir.enums
            .iter()
            .find(|e| e.name == b)
            .map(|e| sanitize(&e.name))
    };
    for e in &ir.enums {
        let n = sanitize(&e.name);
        let _ = writeln!(out, "enum {n} {{");
        for v in &e.values {
            // `created [note: 'x']` → `created // [note: 'x']`; first word is the value
            let (word, rest) = v.split_once(char::is_whitespace).unwrap_or((v, ""));
            let ident = sanitize(word);
            let map = if ident != word {
                format!(" @map({})", quote(word))
            } else {
                String::new()
            };
            let tail = if rest.trim().is_empty() {
                String::new()
            } else {
                format!(" // {}", rest.trim())
            };
            let _ = writeln!(out, "  {ident}{map}{tail}");
        }
        if n != e.name {
            let _ = writeln!(out, "\n  @@map({})", quote(&e.name));
        }
        let _ = writeln!(out, "}}\n");
    }

    // field-name bookkeeping so relation fields never collide with columns
    let mut used: Vec<HashSet<String>> = ir
        .tables
        .iter()
        .map(|t| t.columns.iter().map(|c| sanitize(&c.name)).collect())
        .collect();
    let tidx = |name: &str| ir.tables.iter().position(|t| t.name == name);
    let col_field = |t: &Table, c: &str| {
        let c = c.trim().trim_matches('"');
        sanitize(
            t.columns
                .iter()
                .find(|x| x.name == c)
                .map_or(c, |x| &x.name),
        )
    };

    // relation fields, per table, in ref order
    let mut extra: Vec<Vec<String>> = vec![Vec::new(); ir.tables.len()];
    let mut pair_count: HashMap<(usize, usize), usize> = HashMap::new();
    let mut oriented: Vec<_> = ir
        .refs
        .iter()
        .filter_map(|r| {
            // `<` has the FK on the right-hand table
            let (ft, fc, pt, pc) = if r.kind == RefKind::OneToMany {
                (&r.to_table, &r.to_cols, &r.from_table, &r.from_cols)
            } else {
                (&r.from_table, &r.from_cols, &r.to_table, &r.to_cols)
            };
            let (fi, pi) = (tidx(ft)?, tidx(pt)?);
            *pair_count.entry((fi.min(pi), fi.max(pi))).or_default() += 1;
            Some((r, fi, fc, pi, pc))
        })
        .collect();
    // canonical order (by FK model, stable) — the same order `parse` yields refs in,
    // so emit(parse(emit(ir))) == emit(ir)
    oriented.sort_by_key(|o| o.1);
    for (r, fi, fc, pi, pc) in &oriented {
        let (ft, pt) = (&ir.tables[*fi], &ir.tables[*pi]);
        let (fm, pm) = (sanitize(&ft.name), sanitize(&pt.name));
        let fk: Vec<String> = fc.iter().map(|c| col_field(ft, c)).collect();
        let refs: Vec<String> = pc.iter().map(|c| col_field(pt, c)).collect();
        let ambiguous = fi == pi || pair_count[&(*fi.min(pi), *fi.max(pi))] > 1;
        let rname = r
            .name
            .clone()
            .or_else(|| ambiguous.then(|| auto_rel_name(&fm, &fk)));
        let optional = fc.iter().any(|c| {
            let c = c.trim().trim_matches('"');
            ft.columns.iter().any(|x| x.name == c && !x.not_null)
        });
        let unique_fk = {
            let set: HashSet<&str> = fc.iter().map(|c| c.trim().trim_matches('"')).collect();
            let pk_set: HashSet<&str> = ft
                .columns
                .iter()
                .filter(|c| c.pk)
                .map(|c| c.name.as_str())
                .collect();
            (!pk_set.is_empty() && pk_set == set)
                || ft
                    .columns
                    .iter()
                    .any(|c| c.unique && set.len() == 1 && set.contains(c.name.as_str()))
                || ft.indexes.iter().any(|i| {
                    i.unique
                        && i.expr.is_none()
                        && i.columns.len() == set.len()
                        && i.columns
                            .iter()
                            .all(|c| set.contains(c.trim().trim_matches('"')))
                })
        };
        let base = match fk.as_slice() {
            [c] => c
                .strip_suffix("_id")
                .or_else(|| c.strip_suffix("Id"))
                .filter(|s| !s.is_empty())
                .unwrap_or(&pm)
                .to_string(),
            _ => pm.clone(),
        };
        let rel = unique_name(base, &mut used[*fi]);
        let back = unique_name(fm.clone(), &mut used[*pi]);
        let n = rname
            .as_ref()
            .map_or(String::new(), |n| format!("{}, ", quote(n)));
        extra[*fi].push(format!(
            "{rel} {pm}{} @relation({n}fields: [{}], references: [{}])",
            if optional { "?" } else { "" },
            fk.join(", "),
            refs.join(", ")
        ));
        let back_attr = rname
            .as_ref()
            .map_or(String::new(), |n| format!(" @relation({})", quote(n)));
        extra[*pi].push(format!(
            "{back} {fm}{}{back_attr}",
            if unique_fk { "?" } else { "[]" }
        ));
    }

    for (ti, t) in ir.tables.iter().enumerate() {
        let name = sanitize(&t.name);
        if let Some(n) = &t.note {
            doc_lines(&mut out, "", n);
        }
        let _ = writeln!(out, "model {name} {{");
        let pks: Vec<_> = t.columns.iter().filter(|c| c.pk).collect();
        for c in &t.columns {
            for u in &c.unknown {
                let _ = writeln!(out, "  {UNKNOWN} {u}");
            }
            if let Some(n) = &c.note {
                doc_lines(&mut out, "  ", n);
            }
            let array = c.ty.ends_with("[]");
            let bare = c.ty.strip_suffix("[]").unwrap_or(&c.ty);
            let mapped = ty_to_prisma(bare);
            let (pty, db, serial) = match (enum_name(bare), &mapped) {
                (Some(e), _) => (e, None, false),
                (None, Some((s, d, a))) => (s.to_string(), d.clone(), *a),
                (None, None) => (format!("Unsupported({})", quote(&c.ty)), None, false),
            };
            let unsupported = pty.starts_with("Unsupported");
            let suffix = if unsupported && array {
                ""
            } else if array {
                "[]"
            } else if c.not_null || c.pk {
                ""
            } else {
                "?"
            };
            let _ = write!(out, "  {} {pty}{suffix}", sanitize(&c.name));
            if pks.len() == 1 && c.pk {
                out.push_str(" @id");
            }
            if c.unique {
                out.push_str(" @unique");
            }
            if c.autoincrement || serial {
                out.push_str(" @default(autoincrement())");
            } else if let Some(d) = &c.default {
                let stringy = pty == "String" || pty.starts_with("Unsupported");
                let v = if enum_name(bare).is_some() && is_ident(d) {
                    d.clone() // enum defaults are bare identifiers
                } else {
                    default_to_prisma(d, stringy && !array)
                };
                let _ = write!(out, " @default({v})");
            }
            if let Some(d) = db {
                let _ = write!(out, " @db.{d}");
            }
            if sanitize(&c.name) != c.name {
                let _ = write!(out, " @map({})", quote(&c.name));
            }
            out.push('\n');
        }
        for l in &extra[ti] {
            let _ = writeln!(out, "  {l}");
        }
        let fields = |cols: &[String]| {
            cols.iter()
                .map(|c| col_field(t, c))
                .collect::<Vec<_>>()
                .join(", ")
        };
        if pks.len() > 1 {
            let names: Vec<_> = pks.iter().map(|c| sanitize(&c.name)).collect();
            let _ = writeln!(out, "\n  @@id([{}])", names.join(", "));
        }
        let mut first = pks.len() <= 1;
        for idx in &t.indexes {
            // ponytail: expression/partial indexes have no Prisma form — dropped; the
            // registry reports it via Caps::expr_indexes = false.
            if idx.expr.is_some() || idx.columns.is_empty() {
                continue;
            }
            if first {
                out.push('\n');
                first = false;
            }
            let kind = if idx.unique { "@@unique" } else { "@@index" };
            let map = idx
                .name
                .as_ref()
                .map_or(String::new(), |n| format!(", map: {}", quote(n)));
            let _ = writeln!(out, "  {kind}([{}]{map})", fields(&idx.columns));
        }
        if name != t.name {
            let _ = writeln!(out, "  @@map({})", quote(&t.name));
        }
        for u in &t.unknown {
            let _ = writeln!(out, "  {UNKNOWN} {u}");
        }
        let _ = writeln!(out, "}}\n");
    }
    for u in &ir.unknown {
        let _ = writeln!(out, "{UNKNOWN} {u}");
    }
    out
}
