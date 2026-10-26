//! Parser-independent structural pre-scan of SOURCE text. Fidelity measures
//! against these counts so a lossy parser cannot hide its own loss (issue #25).
//!
//! Each scan is deliberately dumber than the real parser and conservative: it
//! may UNDER-count (weakens the check) but must never OVER-count a correct
//! parse (false alarm). Pure text analysis — no I/O, WASM-safe.

use serde_json::Value;

/// Entity counts declared by a source text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceCounts {
    pub tables: usize,
    pub columns: usize,
    pub refs: usize,
    pub indexes: usize,
}

/// Blank out '…', "…" and `…` contents so keywords inside strings never count.
fn blank_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut q: Option<char> = None;
    for c in s.chars() {
        match q {
            Some(e) if c == e => {
                q = None;
                out.push(c);
            }
            Some(_) => out.push(' '),
            None => {
                if matches!(c, '\'' | '"' | '`') {
                    q = Some(c);
                }
                out.push(c);
            }
        }
    }
    out
}

fn depth_delta(s: &str) -> i32 {
    s.matches('{').count() as i32 - s.matches('}').count() as i32
}

/// DBML: `Table x {` blocks, depth-1 lines as columns, `indexes {}` lines,
/// `Ref` blocks/lines and inline `ref:` settings.
// ponytail: one-line `Table a { id int }` bodies are not column-counted (under-count only).
pub fn dbml(src: &str) -> SourceCounts {
    let mut n = SourceCounts::default();
    let (mut depth, mut in_table, mut in_ref, mut in_idx) = (0i32, false, false, false);
    let (mut in_triple, mut bracket) = (false, 0i32);
    for raw in src.lines() {
        if raw.matches("'''").count() % 2 == 1 {
            in_triple = !in_triple;
            continue;
        }
        if in_triple {
            continue;
        }
        let line = blank_quoted(raw.split("//").next().unwrap_or(""));
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        let lower = l.to_ascii_lowercase();
        let open = l.contains('{');
        match depth {
            0 => {
                if lower.starts_with("table ") && open {
                    n.tables += 1;
                    in_table = true;
                    // first column may share the opener line (`Table t { id int`)
                    let tail = l.split_once('{').map_or("", |(_, t)| t.trim());
                    if !tail.is_empty() && !tail.contains(['{', '}']) && !tail.starts_with("[") {
                        let tl = tail.to_ascii_lowercase();
                        if !tl.starts_with("note") && !tl.starts_with("indexes") {
                            n.columns += 1;
                            n.refs += tl.matches("ref:").count();
                        }
                    }
                } else if lower.starts_with("ref:") {
                    n.refs += 1;
                } else if lower.starts_with("ref") && open {
                    in_ref = true;
                }
            }
            1 if in_ref => {
                if l != "}" {
                    n.refs += 1;
                }
            }
            1 if in_table && bracket == 0 => {
                if lower.starts_with("indexes") && open {
                    in_idx = true;
                } else if !(l.starts_with('}') || lower.starts_with("note") || open) {
                    n.columns += 1;
                    n.refs += lower.matches("ref:").count();
                }
            }
            2 if in_table && in_idx && l != "}" => n.indexes += 1,
            _ => {}
        }
        if depth == 1 && in_table && bracket > 0 {
            n.refs += lower.matches("ref:").count();
        }
        if depth == 1 && in_table {
            bracket += l.matches('[').count() as i32 - l.matches(']').count() as i32;
            bracket = bracket.max(0);
        }
        depth += depth_delta(l);
        if depth <= 1 {
            in_idx = false;
        }
        if depth <= 0 {
            depth = 0;
            in_table = false;
            in_ref = false;
        }
    }
    n
}

/// SQL (all dialects): `CREATE TABLE` bodies split on top-level commas,
/// `REFERENCES` tokens, `CREATE [UNIQUE] INDEX` statements.
pub fn sql(src: &str) -> SourceCounts {
    // strip comments, then quoted text
    let mut clean = String::new();
    let mut rest = src;
    while let Some(i) = rest.find("/*") {
        clean.push_str(&rest[..i]);
        rest = rest[i..].split_once("*/").map_or("", |(_, r)| r);
    }
    clean.push_str(rest);
    let clean: String = clean
        .lines()
        .map(|l| l.split("--").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    let clean = blank_quoted(&clean).to_ascii_uppercase();
    let toks: Vec<&str> = clean
        .split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == ',' || c == ';')
        .filter(|t| !t.is_empty())
        .collect();

    let mut n = SourceCounts::default();
    let mut i = 0;
    while i < toks.len() {
        match toks[i] {
            "REFERENCES" => n.refs += 1,
            "CREATE" => {
                let mut j = i + 1;
                while j < toks.len()
                    && matches!(
                        toks[j],
                        "TEMP" | "TEMPORARY" | "UNLOGGED" | "GLOBAL" | "LOCAL"
                    )
                {
                    j += 1;
                }
                if toks.get(j) == Some(&"TABLE") {
                    n.tables += 1;
                } else if toks.get(j) == Some(&"INDEX")
                    || (toks.get(j) == Some(&"UNIQUE") && toks.get(j + 1) == Some(&"INDEX"))
                {
                    n.indexes += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    n.columns = sql_columns(&clean);
    n
}

fn sql_columns(clean: &str) -> usize {
    const NON_COLUMN: [&str; 11] = [
        "PRIMARY",
        "FOREIGN",
        "UNIQUE",
        "CONSTRAINT",
        "CHECK",
        "KEY",
        "INDEX",
        "FULLTEXT",
        "SPATIAL",
        "EXCLUDE",
        "LIKE",
    ];
    let mut total = 0;
    let mut from = 0;
    while let Some(p) = clean[from..].find("CREATE") {
        let start = from + p;
        from = start + 6;
        let stmt = &clean[start..];
        let stmt = stmt.split(';').next().unwrap_or(stmt);
        let words: Vec<&str> = stmt.split_whitespace().take(3).collect();
        let is_table = words.iter().skip(1).any(|w| *w == "TABLE");
        let Some(open) = stmt.find('(') else { continue };
        let head = &stmt[..open];
        if !is_table || head.contains(" AS ") || head.contains(" LIKE ") {
            continue;
        }
        let (mut depth, mut item) = (0, String::new());
        let mut items = Vec::new();
        for c in stmt[open + 1..].chars() {
            match c {
                '(' => depth += 1,
                ')' if depth == 0 => break,
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    items.push(std::mem::take(&mut item));
                    continue;
                }
                _ => {}
            }
            item.push(c);
        }
        items.push(item);
        total += items
            .iter()
            .filter_map(|it| it.split_whitespace().next())
            .filter(|w| !NON_COLUMN.contains(w))
            .count();
    }
    total
}

/// Prisma: `model` blocks; scalar-typed field lines; `@relation(fields:…)`;
/// `@@index`/`@@unique`.
// ponytail: enum-typed fields are not column-counted (can't tell them from relations without a model table).
pub fn prisma(src: &str) -> SourceCounts {
    const SCALARS: [&str; 10] = [
        "String",
        "Int",
        "BigInt",
        "Float",
        "Decimal",
        "Boolean",
        "DateTime",
        "Json",
        "Bytes",
        "Unsupported",
    ];
    let mut n = SourceCounts::default();
    let (mut depth, mut in_model) = (0i32, false);
    for raw in src.lines() {
        let line = blank_quoted(raw.split("//").next().unwrap_or(""));
        let l = line.trim();
        if depth == 0 && l.starts_with("model ") && l.contains('{') {
            n.tables += 1;
            in_model = true;
        } else if depth == 1 && in_model {
            if l.starts_with("@@index") || l.starts_with("@@unique") {
                n.indexes += 1;
            } else if !l.starts_with('@') && !l.starts_with('}') {
                let mut it = l.split_whitespace();
                let _name = it.next();
                let ty = it.next().unwrap_or("");
                let base = ty
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(['?', '[', ']']);
                if SCALARS.contains(&base) {
                    n.columns += 1;
                }
                if l.contains("@relation(") && l.contains("fields:") {
                    n.refs += 1;
                }
            }
        }
        depth += depth_delta(l);
        if depth <= 0 {
            depth = 0;
            in_model = false;
        }
    }
    n
}

/// ERD-JSON: raw `serde_json::Value` walk (independent of the codec's typed Doc).
pub fn erdjson(src: &str) -> Option<SourceCounts> {
    let v: Value = serde_json::from_str(src).ok()?;
    let arr = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let nodes = arr(&v, "nodes");
    Some(SourceCounts {
        tables: nodes.len(),
        columns: nodes.iter().map(|n| arr(n, "columns").len()).sum(),
        refs: arr(&v, "edges").len(),
        indexes: nodes.iter().map(|n| arr(n, "indexes").len()).sum(),
    })
}
