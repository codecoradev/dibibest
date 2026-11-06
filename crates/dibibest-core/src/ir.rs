//! The intermediate representation. Every format converts IR ↔ text; the IR is
//! the only thing codecs talk to, so adding a format never touches another one.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SchemaIR {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<Table>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<Ref>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enums: Vec<EnumDef>,
    #[serde(default)]
    pub project: Option<String>,
    /// Verbatim lines of the `Project { … }` body (settings, notes).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub project_body: Vec<String>,
    /// Passthrough: schema-level settings the codec did not recognize (e.g. Ref
    /// settings like `delete: cascade`). Re-emitted as `// unknown: …`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Table {
    pub name: String,
    #[serde(default)]
    pub columns: Vec<Column>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<IndexDef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Passthrough: table-/index-level settings the codec did not recognize.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub ty: String,
    #[serde(default)]
    pub pk: bool,
    #[serde(default)]
    pub not_null: bool,
    #[serde(default)]
    pub unique: bool,
    #[serde(default)]
    pub autoincrement: bool,
    /// Default as SQL expression text: string literals keep their single quotes
    /// (`'free'`, inner quotes doubled), numbers / booleans / `NULL` are bare, and
    /// functions or other expressions are verbatim (`now()`, `'{}'::jsonb`). Use
    /// [`sql_string_literal`] / [`as_sql_string`] to convert string literals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Passthrough: column settings the codec did not recognize, verbatim.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexDef {
    pub name: Option<String>,
    /// Key when it is plain columns only (no expressions, no ASC/DESC).
    pub columns: Vec<String>,
    #[serde(default)]
    pub unique: bool,
    /// Key when it is not plain columns: the SQL key list, e.g.
    /// `(lower(email))`, `account_id, id DESC`, `(coalesce(v, 0)), ts`.
    /// Expressions are parenthesised; no DBML backticks. Exactly one of
    /// `columns` / `expr` holds the key (both empty only for a key-less index).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expr: Option<String>,
    /// Partial-index predicate (`WHERE …` without the keyword).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
}

/// One element of an index key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyPart {
    /// A column, optionally with an ordering suffix (`DESC`, `ASC NULLS LAST`).
    Column(String, Option<String>),
    /// An expression (without its surrounding parentheses), optionally ordered.
    Expr(String, Option<String>),
}

/// Split `s` on `sep` at top level: outside '…', "…", `…` and parentheses.
pub fn split_top_level(s: &str, sep: char) -> Vec<String> {
    let (mut out, mut cur, mut depth) = (Vec::new(), String::new(), 0i32);
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '\'' | '"' | '`' => quote = Some(c),
                '(' => depth += 1,
                ')' => depth -= 1,
                _ if c == sep && depth == 0 => {
                    out.push(cur.trim().to_string());
                    cur.clear();
                    continue;
                }
                _ => {}
            },
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() || !out.is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// Byte offset of a top-level, case-insensitive ` WHERE ` keyword in `s`.
fn find_top_level_where(s: &str) -> Option<usize> {
    let (mut depth, mut quote) = (0i32, None::<char>);
    let lower = s.to_ascii_lowercase();
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '\'' | '"' | '`' => quote = Some(c),
                '(' => depth += 1,
                ')' => depth -= 1,
                _ if depth == 0 && lower[i..].starts_with("where") => {
                    let before = i == 0 || s[..i].ends_with(char::is_whitespace);
                    let after = lower[i + 5..].starts_with(char::is_whitespace);
                    if before && after {
                        return Some(i);
                    }
                }
                _ => {}
            },
        }
    }
    None
}

/// `key WHERE pred` → (`key`, Some(`pred`)); no top-level WHERE → (`s`, None).
pub fn split_where(s: &str) -> (String, Option<String>) {
    match find_top_level_where(s) {
        Some(i) => (
            s[..i].trim().to_string(),
            Some(s[i + 5..].trim().to_string()),
        ),
        None => (s.trim().to_string(), None),
    }
}

/// Peel a trailing `ASC`/`DESC` and/or `NULLS FIRST|LAST` off one key element.
/// The body keeps its text as written; the order words are upper-cased.
fn split_order(e: &str) -> (String, Option<String>) {
    fn last_word(s: &str) -> Option<(&str, String)> {
        let (rest, w) = s.rsplit_once(char::is_whitespace)?;
        Some((rest.trim_end(), w.to_ascii_uppercase()))
    }
    let mut body = e.trim();
    let mut suffix: Vec<String> = Vec::new();
    if let Some((rest, w)) = last_word(body).filter(|(_, w)| w == "FIRST" || w == "LAST") {
        if let Some((rest2, _)) = last_word(rest).filter(|(_, n)| n == "NULLS") {
            suffix = vec!["NULLS".into(), w];
            body = rest2;
        }
    }
    if let Some((rest, w)) = last_word(body).filter(|(_, w)| w == "ASC" || w == "DESC") {
        suffix.insert(0, w);
        body = rest;
    }
    if suffix.is_empty() || body.is_empty() {
        (e.trim().to_string(), None)
    } else {
        (body.to_string(), Some(suffix.join(" ")))
    }
}

fn is_plain_ident(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Strip one pair of parentheses that wraps the whole of `s` — only when the
/// opening paren closes at the very end (`(a) + (b)` is not wrapped).
fn strip_wrapping_parens(s: &str) -> Option<&str> {
    let inner = s.strip_prefix('(')?.strip_suffix(')')?;
    let (mut depth, mut quote) = (0i32, None::<char>);
    for c in inner.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '\'' | '"' => quote = Some(c),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth < 0 {
                        return None;
                    }
                }
                _ => {}
            },
        }
    }
    (depth == 0).then_some(inner)
}

/// One SQL key element (`(expr) DESC`, `"my col"`, `lower(x)`) → [`KeyPart`].
pub fn parse_key_part(e: &str) -> KeyPart {
    let (body, order) = split_order(e);
    if let Some(inner) = strip_wrapping_parens(&body) {
        return KeyPart::Expr(inner.trim().to_string(), order);
    }
    if is_plain_ident(&body) {
        return KeyPart::Column(body, order);
    }
    // `col opclass` (e.g. `email text_pattern_ops`): a column with an operator
    // class, not an expression — keep the class in the suffix, before the order
    if let Some((col, class)) = body.split_once(char::is_whitespace) {
        let class = class.trim();
        if is_plain_ident(col) && class.split('.').all(is_plain_ident) {
            let suffix = match order {
                Some(o) => format!("{class} {o}"),
                None => class.to_string(),
            };
            return KeyPart::Column(col.to_string(), Some(suffix));
        }
    }
    for qc in ['"', '`'] {
        if let Some(q) = body.strip_prefix(qc).and_then(|x| x.strip_suffix(qc)) {
            let pair: String = [qc, qc].iter().collect();
            if !q.replace(&pair, "").contains(qc) {
                return KeyPart::Column(q.replace(&pair, &qc.to_string()), order);
            }
        }
    }
    KeyPart::Expr(body, order)
}

/// The key of an index as parts, from `columns` or the `expr` key list.
pub fn index_key_parts(idx: &IndexDef) -> Vec<KeyPart> {
    match &idx.expr {
        Some(e) => split_top_level(e, ',')
            .iter()
            .map(|p| parse_key_part(p))
            .collect(),
        None => idx
            .columns
            .iter()
            .map(|c| KeyPart::Column(c.clone(), None))
            .collect(),
    }
}

/// Store `parts` on `idx`: plain columns go to `columns`, anything else to the
/// canonical SQL key list in `expr` (expressions parenthesised).
pub fn set_index_key(idx: &mut IndexDef, parts: Vec<KeyPart>) {
    if parts.iter().all(|p| matches!(p, KeyPart::Column(_, None))) {
        idx.columns = parts
            .into_iter()
            .map(|p| match p {
                KeyPart::Column(c, _) => c,
                KeyPart::Expr(..) => unreachable!(),
            })
            .collect();
        idx.expr = None;
        return;
    }
    let render = |p: &KeyPart| {
        let (body, order) = match p {
            KeyPart::Column(c, o) => (
                if is_plain_ident(c) {
                    c.clone()
                } else {
                    format!("\"{}\"", c.replace('"', "\"\""))
                },
                o,
            ),
            KeyPart::Expr(e, o) => (format!("({e})"), o),
        };
        match order {
            Some(o) => format!("{body} {o}"),
            None => body,
        }
    };
    idx.columns = Vec::new();
    idx.expr = Some(parts.iter().map(render).collect::<Vec<_>>().join(", "));
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ref {
    pub from_table: String,
    pub from_cols: Vec<String>,
    pub to_table: String,
    pub to_cols: Vec<String>,
    pub kind: RefKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Referential action on delete, normalized by [`normalize_action`]
    /// (`cascade`, `set null`, `set default`, `restrict`, `no action`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_delete: Option<String>,
    /// Referential action on update, same vocabulary as `on_delete`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_update: Option<String>,
    /// The `from` side may have no matching row (DBML `?>` / `?-`, dbdiagram
    /// optional relationships). Presentation-level: SQL and Prisma express
    /// optionality through nullability only, so they report it as dropped.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional_from: bool,
    /// The `to` side may have no matching row (DBML `>?` / `<?`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional_to: bool,
}

/// Normalize a referential action from any codec's spelling (`CASCADE`,
/// `set null`, `SetNull`, `set_null`, `NoAction`, …) to the IR vocabulary.
/// Returns `None` for anything that is not a known action.
pub fn normalize_action(s: &str) -> Option<String> {
    let k: String = s
        .trim()
        .trim_matches(|c| c == '\'' || c == '"')
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase();
    Some(
        match k.as_str() {
            "cascade" => "cascade",
            "restrict" => "restrict",
            "setnull" => "set null",
            "setdefault" => "set default",
            "noaction" => "no action",
            _ => return None,
        }
        .to_string(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RefKind {
    ManyToOne,
    OneToOne,
    OneToMany,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EnumDef {
    pub name: String,
    #[serde(default)]
    pub values: Vec<String>,
}

impl SchemaIR {
    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables.iter().find(|t| t.name == name)
    }
}

/// A SQL string literal for `s`: single-quoted, inner `'` doubled.
pub fn sql_string_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// If `d` is exactly one SQL string literal (`'it''s'`), its unescaped content.
/// Expressions that merely contain literals (`'{}'::jsonb`) return `None`.
pub fn as_sql_string(d: &str) -> Option<String> {
    let inner = d.strip_prefix('\'')?.strip_suffix('\'')?;
    // every quote inside must be part of a doubled `''` pair
    let mut out = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\'' && chars.next() != Some('\'') {
            return None;
        }
        out.push(c);
    }
    Some(out)
}

/// A default that every format writes bare: a number, `true`/`false` or `null`.
pub fn is_bare_literal(d: &str) -> bool {
    let l = d.to_ascii_lowercase();
    l == "true" || l == "false" || l == "null" || d.parse::<f64>().is_ok()
}
