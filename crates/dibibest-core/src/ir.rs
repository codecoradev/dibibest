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
    pub columns: Vec<String>,
    #[serde(default)]
    pub unique: bool,
    /// DBML expression indexes (e.g. partial indexes) — kept verbatim when a
    /// target format cannot represent them, so round-trips stay lossless.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expr: Option<String>,
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
