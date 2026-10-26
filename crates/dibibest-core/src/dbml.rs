//! DBML codec (dbdiagram.io dialect) — built-in parser + emitter.
//!
//! Scaffold subset (v0.1): Project, Table (columns + settings, indexes incl.
//! expressions, notes), Ref (m2o/o2o/1m, composite, inline, blocks), Enum.
//! Unrecognized settings are preserved as `// unknown: …` comments. Corpus
//! conformance lives in `tests/corpus`; gaps in `docs/dbml-gaps.md`.

use crate::ir::*;
use crate::{Caps, Codec, CoreError};
use std::fmt::Write as _;

pub struct DbmlCodec;

const FMT: &str = "dbml";

impl Codec for DbmlCodec {
    fn scan(&self, src: &str) -> Option<crate::SourceCounts> {
        Some(crate::scan::dbml(src))
    }

    fn id(&self) -> &str {
        "dbml"
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

    fn parse(&self, src: &str) -> Result<SchemaIR, CoreError> {
        parse_dbml(src)
    }

    fn emit(&self, ir: &SchemaIR) -> Result<String, CoreError> {
        emit_dbml(ir)
    }
}

// ── Parse ───────────────────────────────────────────────────────────────────

const UNKNOWN: &str = "// unknown:";

/// Split one-line blocks (`Table a { id integer [pk] }`) into three virtual
/// lines so the line-based state machine handles agent-style one-liners.
fn expand_one_line_blocks(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in src.lines() {
        let t = raw.trim();
        let opener = t.starts_with("Table ")
            || t.starts_with("Enum ")
            || t.starts_with("Project ")
            || (t.starts_with("Ref ") && t.find('{') < t.find(':').or(Some(usize::MAX)));
        if opener && t.ends_with('}') && t.contains('{') {
            let brace = t.find('{').unwrap();
            let header = t[..=brace].trim_end();
            let body = t[brace + 1..t.len() - 1].trim();
            out.push(header.to_string());
            if !body.is_empty() {
                out.push(body.to_string());
            }
            out.push("}".to_string());
        } else {
            out.push(raw.to_string());
        }
    }
    out
}

/// Map a dbml parse-failure message to its stable code (`docs/errors.md`).
pub(crate) fn error_code(message: &str) -> &'static str {
    let has = |s: &str| message.contains(s);
    if has("block not closed") {
        "unclosed-block"
    } else if has("statement `}`") {
        "unbalanced-brace"
    } else if has("bad index line") {
        "bad-index-line"
    } else if has("bad column line") {
        "bad-column-line"
    } else if has("Ref line") || has("bad inline ref") {
        "bad-ref-line"
    } else {
        // unsupported top-level statement / nested block / alias / ''' note
        "unrecognized-statement"
    }
}

fn parse_dbml(src: &str) -> Result<SchemaIR, CoreError> {
    let mut ir = SchemaIR::default();
    let mut block: Option<Block> = None;

    for (lineno, raw) in expand_one_line_blocks(src).iter().enumerate() {
        let line = raw.trim();
        let fail = |m: &str| CoreError::Parse {
            format: FMT,
            message: format!("line {}: {}", lineno + 1, m),
        };

        let mut closed = false;
        match (&mut block, line) {
            // indexes sub-block closer must not close the whole table
            (Some(Block::Table(ts)), "}") if ts.index_state => {
                ts.index_state = false;
            }
            // nested braces inside a Project body (e.g. `Note { … }`)
            (Some(Block::Project(p)), "}") if p.depth > 0 => {
                p.depth -= 1;
                p.body.push("}".into());
            }
            // ── inside a block ──
            (Some(b), "}") => {
                match b {
                    Block::Table(t) => {
                        let mut table = std::mem::take(&mut t.table);
                        table.unknown.append(&mut t.pending);
                        ir.tables.push(table);
                    }
                    Block::Enum(e) => ir.enums.push(std::mem::take(e)),
                    Block::Project(p) => {
                        ir.project = Some(std::mem::take(&mut p.name));
                        ir.project_body = std::mem::take(&mut p.body);
                    }
                    Block::Ref(_) => {}
                }
                closed = true;
            }
            (Some(Block::Table(ts)), l) if l.starts_with(UNKNOWN) => {
                ts.pending.push(l[UNKNOWN.len()..].trim().to_string());
            }
            (Some(Block::Table(ts)), l) if !l.is_empty() && !l.starts_with("//") => {
                let l = strip_comment(l);
                if l.is_empty() {
                    continue;
                }
                if l.strip_prefix("indexes").is_some_and(|r| r.trim() == "{") {
                    ts.index_state = true;
                    continue;
                }
                if ts.index_state {
                    // `(...)` with settings inline — parse_index_line applies both.
                    let (idx, unk) = parse_index_line(l).ok_or_else(|| fail("bad index line"))?;
                    ts.table.indexes.push(idx);
                    ts.table.unknown.extend(unk);
                    continue;
                }
                if l.to_ascii_lowercase().starts_with("note:") {
                    let v = l["note:".len()..].trim();
                    if v.starts_with("'''") {
                        return Err(fail("multi-line ''' note is unsupported"));
                    }
                    ts.table.note = Some(unquote_str(v));
                    continue;
                }
                if l.ends_with('{') {
                    return Err(fail("nested table block is unsupported"));
                }
                let (mut col, refs) =
                    parse_column_line(l).ok_or_else(|| fail("bad column line"))?;
                col.unknown.append(&mut ts.pending);
                for spec in refs {
                    // inline `ref: > t.c` is the same Ref as a top-level one
                    let body = format!(
                        "{}.{} {}",
                        quote_ident(&ts.table.name),
                        quote_ident(&col.name),
                        spec
                    );
                    let (r, unk) = parse_ref_body(&body).ok_or_else(|| fail("bad inline ref"))?;
                    ir.refs.push(r);
                    ir.unknown.extend(unk);
                }
                ts.table.columns.push(col);
            }
            (Some(Block::Enum(e)), l) if !l.is_empty() => {
                let v = strip_comment(l);
                if !v.is_empty() {
                    e.values.push(v.trim_matches(',').to_string());
                }
            }
            (Some(Block::Project(p)), l) if !l.is_empty() && !l.starts_with("//") => {
                if l.ends_with('{') {
                    p.depth += 1;
                }
                p.body.push(l.to_string());
            }
            (Some(Block::Ref(name)), l) if !l.is_empty() && !l.starts_with("//") => {
                let (mut r, unk) =
                    parse_ref_body(strip_comment(l)).ok_or_else(|| fail("bad Ref line"))?;
                r.name = name.clone();
                ir.refs.push(r);
                ir.unknown.extend(unk);
            }
            (Some(_), _) => {}

            // ── outside any block ──
            (None, l) if l.starts_with(UNKNOWN) => {
                ir.unknown.push(l[UNKNOWN.len()..].trim().to_string());
            }
            (None, l) if l.is_empty() || l.starts_with("//") => {}
            (None, l) if l.starts_with("Project ") => {
                let name = l
                    .strip_prefix("Project ")
                    .and_then(|s| s.split('{').next())
                    .unwrap_or("")
                    .trim();
                block = Some(Block::Project(ProjectState {
                    name: unquote_ident(name),
                    depth: 0,
                    body: Vec::new(),
                }));
            }
            (None, l) if l.starts_with("Table ") => {
                block = Some(Block::Table(TableState {
                    table: parse_table_header(l).map_err(|m| fail(&m))?,
                    index_state: false,
                    pending: Vec::new(),
                }));
            }
            (None, l) if l.starts_with("Enum ") => {
                let name = l
                    .strip_prefix("Enum ")
                    .and_then(|s| s.split('{').next())
                    .unwrap_or("")
                    .trim();
                block = Some(Block::Enum(EnumDef {
                    name: unquote_ident(name),
                    values: Vec::new(),
                }));
            }
            (None, l) if l == "Ref" || l.starts_with("Ref ") || l.starts_with("Ref:") => {
                let rest = l["Ref".len()..].trim_start();
                let Some((i, sep)) = outside_quotes(rest).find(|&(_, c)| c == ':' || c == '{')
                else {
                    return Err(fail("bad Ref line"));
                };
                let head = rest[..i].trim();
                let name = (!head.is_empty()).then(|| unquote_ident(head));
                if sep == '{' {
                    block = Some(Block::Ref(name));
                } else {
                    let (mut r, unk) = parse_ref_body(strip_comment(rest[i + 1..].trim()))
                        .ok_or_else(|| fail("bad Ref line"))?;
                    r.name = name;
                    ir.refs.push(r);
                    ir.unknown.extend(unk);
                }
            }
            (None, l) => {
                let kw = l.split_whitespace().next().unwrap_or(l);
                return Err(fail(&format!("unsupported top-level statement `{kw}`")));
            }
        }
        if closed {
            block = None;
        }
    }
    if block.is_some() {
        return Err(CoreError::Parse {
            format: FMT,
            message: "unexpected end of input: block not closed".into(),
        });
    }
    Ok(ir)
}

enum Block {
    Project(ProjectState),
    Table(TableState),
    Enum(EnumDef),
    Ref(Option<String>),
}

struct ProjectState {
    name: String,
    depth: usize,
    body: Vec<String>,
}

struct TableState {
    table: Table,
    index_state: bool,
    /// `// unknown:` lines seen since the last column; they belong to the next
    /// column, or to the table if none follows.
    pending: Vec<String>,
}

/// Characters of `s` outside `'…'`, `"…"` and `` `…` `` (backslash escapes the
/// next char inside a quote), with their byte offsets. Quote chars themselves
/// are skipped.
fn outside_quotes(s: &str) -> impl Iterator<Item = (usize, char)> + '_ {
    let mut quote: Option<char> = None;
    let mut esc = false;
    s.char_indices().filter(move |&(_, ch)| {
        if esc {
            esc = false;
            return false;
        }
        match quote {
            Some(q) => {
                if ch == '\\' {
                    esc = true;
                } else if ch == q {
                    quote = None;
                }
                false
            }
            None if matches!(ch, '\'' | '"' | '`') => {
                quote = Some(ch);
                false
            }
            None => true,
        }
    })
}

fn strip_comment(l: &str) -> &str {
    let v: Vec<_> = outside_quotes(l).collect();
    match v
        .windows(2)
        .find(|w| w[0].1 == '/' && w[1].1 == '/' && w[1].0 == w[0].0 + 1)
    {
        Some(w) => l[..w[0].0].trim(),
        None => l,
    }
}

fn unquote_ident(s: &str) -> String {
    s.trim().replace('"', "")
}

/// `'text with \' escape'` → `text with ' escape`.
fn unquote_str(v: &str) -> String {
    let v = v.trim();
    let inner = v
        .strip_prefix('\'')
        .and_then(|x| x.strip_suffix('\''))
        .unwrap_or(v);
    inner.replace("\\'", "'")
}

fn quote_str(v: &str) -> String {
    format!("'{}'", v.replace('\'', "\\'"))
}

/// Settings bracket body `[ … ]` → inner text, if `s` is exactly one.
fn bracket_inner(s: &str) -> Option<&str> {
    s.trim().strip_prefix('[')?.strip_suffix(']')
}

/// `Table name [settings] {` → Table with header settings preserved as unknown.
fn parse_table_header(l: &str) -> Result<Table, String> {
    let rest = l.strip_prefix("Table ").unwrap_or(l);
    let brace = outside_quotes(rest)
        .find(|&(_, c)| c == '{')
        .map(|x| x.0)
        .ok_or("Table header without `{`")?;
    let head = rest[..brace].trim();
    let (name, settings) = match outside_quotes(head).find(|&(_, c)| c == '[') {
        Some((i, _)) => (head[..i].trim(), bracket_inner(&head[i..])),
        None => (head, None),
    };
    let visible: String = outside_quotes(name).map(|x| x.1).collect();
    if name.is_empty() || visible.contains(" as ") {
        return Err("Table alias (`as X`) / missing name is unsupported".into());
    }
    let mut t = Table {
        name: unquote_ident(name),
        ..Table::default()
    };
    t.unknown = settings.map(split_settings).unwrap_or_default();
    Ok(t)
}

/// Returns the column plus raw inline-ref specs (`> users.id`).
fn parse_column_line(l: &str) -> Option<(Column, Vec<String>)> {
    let (name, rest) = if let Some(r) = l.strip_prefix('"') {
        let end = r.find('"')?;
        (r[..end].to_string(), r[end + 1..].trim())
    } else {
        let mut parts = l.splitn(2, char::is_whitespace);
        (parts.next()?.trim().to_string(), parts.next()?.trim())
    };
    // settings start at the first `[` that follows whitespace (`int[]` is a type)
    let open = outside_quotes(rest)
        .find(|&(i, c)| c == '[' && (i == 0 || rest[..i].ends_with(char::is_whitespace)))
        .map(|x| x.0);
    let (ty, settings) = match open {
        Some(i) => (rest[..i].trim(), bracket_inner(&rest[i..])?),
        None => (rest, ""),
    };
    let mut c = Column {
        name,
        ty: ty.to_string(),
        ..Default::default()
    };
    let mut refs = Vec::new();
    for s in split_settings(settings) {
        match s.as_str() {
            "pk" | "primary key" => c.pk = true,
            "not null" => c.not_null = true,
            "unique" => c.unique = true,
            "increment" | "autoincrement" => c.autoincrement = true,
            _ if s.starts_with("default:") => {
                let v = s["default:".len()..].trim();
                c.default = Some(v.trim_matches(|q| q == '\'' || q == '`').to_string());
            }
            _ if s.starts_with("note:") => c.note = Some(unquote_str(&s["note:".len()..])),
            _ if s.starts_with("ref:") => refs.push(s["ref:".len()..].trim().to_string()),
            _ => c.unknown.push(s),
        }
    }
    if c.pk {
        c.not_null = true; // DBML semantics: pk implies not null
    }
    Some((c, refs))
}

/// Parse `(...) [settings]` — the `(...)` may be a column list or an
/// expression (which can itself contain parens/quotes); settings may follow
/// on the same line. A bare `col [settings]` is a one-column index. Unknown
/// settings are returned for the caller to preserve.
fn parse_index_line(l: &str) -> Option<(IndexDef, Vec<String>)> {
    let (inner, rest) = if l.starts_with('(') {
        let mut depth = 0usize;
        let mut close = None;
        for (i, ch) in outside_quotes(l) {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        close = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let close = close?;
        (&l[1..close], l[close + 1..].trim())
    } else {
        let end = l
            .find(|c: char| c.is_whitespace() || c == '[')
            .unwrap_or(l.len());
        (&l[..end], l[end..].trim())
    };
    let mut idx = IndexDef {
        name: None,
        columns: Vec::new(),
        unique: false,
        expr: None,
    };
    if inner.contains('`') || inner.contains('(') {
        idx.expr = Some(inner.to_string());
    } else {
        idx.columns = inner.split(',').map(|c| c.trim().to_string()).collect();
    }
    let mut unknown = Vec::new();
    if let Some(settings) = bracket_inner(rest) {
        for s in split_settings(settings) {
            if s == "unique" {
                idx.unique = true;
            } else if let Some(n) = s.strip_prefix("name:") {
                idx.name = Some(unquote_str(n));
            } else {
                unknown.push(s);
            }
        }
    } else if !rest.is_empty() {
        return None;
    }
    Some((idx, unknown))
}

/// `a.b > c.d [settings]`, composite `a.(x, y) > c.(x, y)`. Ref settings are
/// all unknown to the IR and returned for preservation.
fn parse_ref_body(body: &str) -> Option<(Ref, Vec<String>)> {
    let body = body.trim();
    let (op, kind) = outside_quotes(body).find_map(|(i, c)| match c {
        '>' => Some((i, RefKind::ManyToOne)),
        '-' => Some((i, RefKind::OneToOne)),
        // `<>` (many-to-many) has no IR kind yet — fail loudly
        '<' => (!body[i + 1..].starts_with('>')).then_some((i, RefKind::OneToMany)),
        _ => None,
    })?;
    let rhs_full = &body[op + 1..];
    let (rhs, unknown) = match outside_quotes(rhs_full).find(|&(_, c)| c == '[') {
        Some((i, _)) => (
            rhs_full[..i].trim(),
            split_settings(bracket_inner(&rhs_full[i..])?),
        ),
        None => (rhs_full.trim(), Vec::new()),
    };
    let (ft, fc) = ref_endpoint(&body[..op])?;
    let (tt, tc) = ref_endpoint(rhs)?;
    Some((
        Ref {
            from_table: ft,
            from_cols: fc,
            to_table: tt,
            to_cols: tc,
            kind,
            name: None,
        },
        unknown,
    ))
}

fn ref_endpoint(s: &str) -> Option<(String, Vec<String>)> {
    let s = s.trim();
    let (table, cols) = match outside_quotes(s).find(|&(_, c)| c == '(') {
        Some((i, _)) => (
            s[..i].trim().strip_suffix('.')?,
            s[i + 1..].strip_suffix(')')?,
        ),
        None => s.rsplit_once('.')?,
    };
    Some((
        unquote_ident(table),
        cols.split(',').map(unquote_ident).collect(),
    ))
}

/// Split DBML settings on commas that are not inside quotes.
fn split_settings(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, _) in outside_quotes(s).filter(|&(_, c)| c == ',') {
        out.push(s[start..i].trim().to_string());
        start = i + 1;
    }
    out.push(s[start..].trim().to_string());
    out.retain(|x| !x.is_empty());
    out
}

// ── Emit ────────────────────────────────────────────────────────────────────

fn emit_dbml(ir: &SchemaIR) -> Result<String, CoreError> {
    let mut out = String::new();
    if let Some(p) = &ir.project {
        let _ = writeln!(out, "Project {} {{", quote_ident(p));
        for l in &ir.project_body {
            let _ = writeln!(out, "  {}", l);
        }
        let _ = writeln!(out, "}}\n");
    }
    for e in &ir.enums {
        let _ = writeln!(out, "Enum {} {{", quote_ident(&e.name));
        for v in &e.values {
            let _ = writeln!(out, "  {}", v);
        }
        let _ = writeln!(out, "}}\n");
    }
    for t in &ir.tables {
        let _ = writeln!(out, "Table {} {{", quote_ident(&t.name));
        for c in &t.columns {
            for u in &c.unknown {
                let _ = writeln!(out, "  {} {}", UNKNOWN, u);
            }
            let _ = write!(out, "  {} {}", quote_ident(&c.name), c.ty);
            let mut settings: Vec<String> = Vec::new();
            if c.pk {
                settings.push("pk".into());
            }
            if c.not_null && !c.pk {
                settings.push("not null".into());
            }
            if c.unique {
                settings.push("unique".into());
            }
            if c.autoincrement {
                settings.push("increment".into());
            }
            if let Some(d) = &c.default {
                settings.push(format!("default: `{}`", d));
            }
            if let Some(n) = &c.note {
                settings.push(format!("note: {}", quote_str(n)));
            }
            if !settings.is_empty() {
                let _ = write!(out, " [{}]", settings.join(", "));
            }
            let _ = writeln!(out);
        }
        if !t.indexes.is_empty() {
            let _ = writeln!(out, "\n  indexes {{");
            for idx in &t.indexes {
                let cols = if idx.columns.is_empty() {
                    format!("({})", idx.expr.as_deref().unwrap_or(""))
                } else {
                    format!("({})", idx.columns.join(", "))
                };
                let mut settings: Vec<String> = Vec::new();
                if idx.unique {
                    settings.push("unique".into());
                }
                if let Some(n) = &idx.name {
                    settings.push(format!("name: {}", quote_str(n)));
                }
                if settings.is_empty() {
                    let _ = writeln!(out, "    {}", cols);
                } else {
                    let _ = writeln!(out, "    {} [{}]", cols, settings.join(", "));
                }
            }
            let _ = writeln!(out, "  }}");
        }
        if let Some(n) = &t.note {
            let _ = writeln!(out, "\n  note: {}", quote_str(n));
        }
        for u in &t.unknown {
            let _ = writeln!(out, "  {} {}", UNKNOWN, u);
        }
        let _ = writeln!(out, "}}\n");
    }
    for r in &ir.refs {
        let op = match r.kind {
            RefKind::ManyToOne => ">",
            RefKind::OneToOne => "-",
            RefKind::OneToMany => "<",
        };
        let name = r
            .name
            .as_ref()
            .map_or(String::new(), |n| format!(" {}", quote_ident(n)));
        let _ = writeln!(
            out,
            "Ref{}: {} {} {}",
            name,
            ref_endpoint_str(&r.from_table, &r.from_cols),
            op,
            ref_endpoint_str(&r.to_table, &r.to_cols)
        );
    }
    for u in &ir.unknown {
        let _ = writeln!(out, "{} {}", UNKNOWN, u);
    }
    Ok(out)
}

fn ref_endpoint_str(table: &str, cols: &[String]) -> String {
    match cols {
        [c] => format!("{}.{}", quote_ident(table), quote_ident(c)),
        _ => {
            let cols: Vec<_> = cols.iter().map(|c| quote_ident(c)).collect();
            format!("{}.({})", quote_ident(table), cols.join(", "))
        }
    }
}

fn quote_ident(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        && s.parse::<f64>().is_err()
    {
        s.to_string()
    } else {
        format!("\"{}\"", s)
    }
}
