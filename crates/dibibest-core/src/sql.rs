//! SQL DDL codec via sqlparser-rs (GenericDialect parse; dialect-specific emit:
//! `sql` = PostgreSQL, `sql:mysql`, `sql:sqlite`).
//! Scaffold subset: CREATE TABLE (columns, PK/NOT NULL/UNIQUE/DEFAULT, inline
//! + table-level FK), CREATE [UNIQUE] INDEX. CHECK/COMMENT/etc = R2 corpus work.

use crate::ir::*;
use crate::{Caps, Codec, CoreError};
use sqlparser::ast::{
    ColumnOption, DataType, Expr, ObjectName, Statement, TableConstraint, TimezoneInfo,
    UserDefinedTypeRepresentation,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;
use std::fmt::Write as _;

/// One impl, three registered instances: the dialect only steers emit + caps.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Dialect {
    #[default]
    Postgres,
    MySql,
    Sqlite,
}

#[derive(Default)]
pub struct SqlCodec {
    pub dialect: Dialect,
}

impl SqlCodec {
    pub fn new(dialect: Dialect) -> Self {
        SqlCodec { dialect }
    }

    /// Quote ONE identifier (column, index, or one part of a table name). A dot
    /// is part of the name here: `"v1.2"` stays a single column.
    /// MySQL: always backticks. PG/SQLite: plain names stay bare (unchanged
    /// output); anything else — spaces, dashes, dots, a leading digit, reserved
    /// words — is double-quoted with inner `"` doubled, so the emitted DDL
    /// re-parses and a name can never break out of its quotes.
    fn q(&self, ident: &str) -> String {
        if self.dialect == Dialect::MySql {
            return format!("`{}`", ident.replace('`', "``"));
        }
        let plain = ident
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !is_reserved(ident);
        if plain {
            ident.to_string()
        } else {
            format!("\"{}\"", ident.replace('"', "\"\""))
        }
    }

    /// Quote a table name, which may be schema-qualified: each dotted part is
    /// quoted separately (`billing.plans` → `` `billing`.`plans` `` on MySQL).
    /// The DBML codec already reads a dotted table name as schema.table (see
    /// docs/dbml-gaps.md), so this matches the IR's meaning.
    fn qt(&self, table: &str) -> String {
        table
            .split('.')
            .map(|p| self.q(p))
            .collect::<Vec<_>>()
            .join(".")
    }

    fn qs(&self, idents: &[String]) -> String {
        idents
            .iter()
            .map(|i| self.q(i))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

const FMT: &str = "sql";

impl Codec for SqlCodec {
    fn scan(&self, src: &str) -> Option<crate::SourceCounts> {
        Some(crate::scan::sql(src))
    }

    fn id(&self) -> &str {
        match self.dialect {
            Dialect::Postgres => "sql",
            Dialect::MySql => "sql:mysql",
            Dialect::Sqlite => "sql:sqlite",
        }
    }

    fn caps(&self) -> Caps {
        let (expr_indexes, enums, dialects): (_, _, &'static [&'static str]) = match self.dialect {
            Dialect::Postgres => (true, true, &["postgresql"]),
            // MySQL: no partial indexes; ENUM is a per-column type, not a named type.
            Dialect::MySql => (false, false, &["mysql"]),
            Dialect::Sqlite => (true, false, &["sqlite"]),
        };
        Caps {
            indexes: true,
            expr_indexes,
            enums,
            defaults: true,
            notes: false, // COMMENT ON: R2
            dialects,
        }
    }

    fn parse(&self, src: &str) -> Result<SchemaIR, CoreError> {
        let dialect = GenericDialect {};
        let stmts = Parser::parse_sql(&dialect, src).map_err(|e| CoreError::Parse {
            format: FMT,
            message: e.to_string(),
        })?;
        let mut ir = SchemaIR::default();
        for stmt in stmts {
            match stmt {
                Statement::CreateTable(ct) => {
                    let name = object_name_to_string(&ct.name);
                    let mut t = Table {
                        name,
                        ..Table::default()
                    };
                    let mut pending_refs: Vec<Ref> = Vec::new();
                    for cd in &ct.columns {
                        let mut c = Column {
                            name: ident_str(&cd.name),
                            ty: render_type(&cd.data_type),
                            ..Column::default()
                        };
                        for opt in &cd.options {
                            match &opt.option {
                                ColumnOption::PrimaryKey(_) => {
                                    c.pk = true;
                                    c.not_null = true;
                                }
                                ColumnOption::Unique(_) => c.unique = true,
                                ColumnOption::NotNull => c.not_null = true,
                                ColumnOption::DialectSpecific(toks)
                                    if toks.iter().any(|t| {
                                        let t = t.to_string().to_ascii_uppercase();
                                        t == "AUTO_INCREMENT" || t == "AUTOINCREMENT"
                                    }) =>
                                {
                                    c.autoincrement = true;
                                }
                                ColumnOption::Default(expr) => {
                                    c.default = Some(expr.to_string());
                                }
                                ColumnOption::ForeignKey(fk) => pending_refs.push(Ref {
                                    from_table: t.name.clone(),
                                    from_cols: vec![c.name.clone()],
                                    to_table: object_name_to_string(&fk.foreign_table),
                                    to_cols: fk.referred_columns.iter().map(ident_str).collect(),
                                    kind: RefKind::ManyToOne,
                                    name: None,
                                    on_delete: action(&fk.on_delete),
                                    on_update: action(&fk.on_update),
                                }),
                                _ => {}
                            }
                        }
                        t.columns.push(c);
                    }
                    for cons in &ct.constraints {
                        match cons {
                            TableConstraint::PrimaryKey(pk) => {
                                for ic in &pk.columns {
                                    if let Expr::Identifier(id) = &ic.column.expr {
                                        if let Some(col) =
                                            t.columns.iter_mut().find(|c| c.name == id.value)
                                        {
                                            col.pk = true;
                                            col.not_null = true;
                                        }
                                    }
                                }
                            }
                            TableConstraint::ForeignKey(fk) => pending_refs.push(Ref {
                                from_table: t.name.clone(),
                                from_cols: fk.columns.iter().map(ident_str).collect(),
                                to_table: object_name_to_string(&fk.foreign_table),
                                to_cols: fk.referred_columns.iter().map(ident_str).collect(),
                                kind: RefKind::ManyToOne,
                                name: None,
                                on_delete: action(&fk.on_delete),
                                on_update: action(&fk.on_update),
                            }),
                            _ => {}
                        }
                    }
                    ir.tables.push(t);
                    ir.refs.extend(pending_refs);
                }
                Statement::CreateType {
                    name,
                    representation: Some(UserDefinedTypeRepresentation::Enum { labels }),
                } => ir.enums.push(EnumDef {
                    name: object_name_to_string(&name),
                    values: labels.iter().map(ident_str).collect(),
                }),
                Statement::CreateIndex(ci) => {
                    let table = object_name_to_string(&ci.table_name);
                    if let Some(t) = ir.tables.iter_mut().find(|t| t.name == table) {
                        let mut idx = IndexDef {
                            name: ci.name.as_ref().map(object_name_to_string),
                            columns: Vec::new(),
                            unique: ci.unique,
                            expr: None,
                            predicate: ci.predicate.as_ref().map(|p| p.to_string()),
                        };
                        // `col`, `col DESC`, `(expr)`, `lower(x)` — same key model
                        // as the DBML codec (see `set_index_key`)
                        let parts = ci.columns.iter().map(index_key_part).collect();
                        set_index_key(&mut idx, parts);
                        t.indexes.push(idx);
                    }
                }
                _ => {}
            }
        }
        Ok(ir)
    }

    fn emit(&self, ir: &SchemaIR) -> Result<String, CoreError> {
        let mut out = String::new();
        // index names are unique per schema: generated names must not collide
        // with explicit ones or with each other (`idx_t_expr`, `idx_t_expr_2`)
        let mut used_index_names: std::collections::HashSet<String> = ir
            .tables
            .iter()
            .flat_map(|t| t.indexes.iter().filter_map(|i| i.name.clone()))
            .collect();
        if self.dialect == Dialect::Postgres {
            for e in &ir.enums {
                let vals: Vec<String> = e
                    .values
                    .iter()
                    .map(|v| format!("'{}'", v.replace('\'', "''")))
                    .collect();
                let _ = writeln!(out, "CREATE TYPE {} AS ENUM ({});", e.name, vals.join(", "));
            }
            if !ir.enums.is_empty() {
                out.push('\n');
            }
        }
        for t in &ir.tables {
            let mut lines: Vec<String> = Vec::new();
            for c in &t.columns {
                let mut s = format!("  {} {}", self.q(&c.name), c.ty);
                if c.pk {
                    s.push_str(" PRIMARY KEY");
                    if c.autoincrement && self.dialect == Dialect::Sqlite {
                        s.push_str(" AUTOINCREMENT");
                    }
                } else if c.not_null {
                    s.push_str(" NOT NULL");
                }
                if c.unique && !c.pk {
                    s.push_str(" UNIQUE");
                }
                if c.autoincrement && self.dialect == Dialect::MySql {
                    s.push_str(" AUTO_INCREMENT");
                }
                if let Some(d) = &c.default {
                    let _ = write!(s, " DEFAULT {}", d);
                }
                lines.push(s);
            }
            let pk_cols: Vec<String> = t
                .columns
                .iter()
                .filter(|c| c.pk)
                .map(|c| c.name.clone())
                .collect();
            if pk_cols.len() > 1 {
                lines.push(format!("  PRIMARY KEY ({})", self.qs(&pk_cols)));
            }
            for r in &ir.refs {
                // The FK lives on the "many" side: for `OneToMany` (DBML `<`) that is
                // `to_table`, otherwise `from_table`.
                let (fk_table, fk_cols, pk_table, pk_cols) = if r.kind == RefKind::OneToMany {
                    (&r.to_table, &r.to_cols, &r.from_table, &r.from_cols)
                } else {
                    (&r.from_table, &r.from_cols, &r.to_table, &r.to_cols)
                };
                if *fk_table != t.name {
                    continue;
                }
                // Omitted referenced columns mean "the target's primary key". If that
                // is unknown, skip the FK (never emit `REFERENCES t()`); the registry's
                // refs fidelity comparison reports the drop.
                let pk_cols = if pk_cols.is_empty() {
                    ir.tables
                        .iter()
                        .find(|x| x.name == *pk_table)
                        .map(|x| {
                            x.columns
                                .iter()
                                .filter(|c| c.pk)
                                .map(|c| c.name.clone())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                } else {
                    pk_cols.clone()
                };
                if pk_cols.is_empty() || pk_cols.len() != fk_cols.len() {
                    continue;
                }
                let mut fk = format!(
                    "  FOREIGN KEY ({}) REFERENCES {}({})",
                    self.qs(fk_cols),
                    self.qt(pk_table),
                    self.qs(&pk_cols)
                );
                for (when, act) in [("DELETE", &r.on_delete), ("UPDATE", &r.on_update)] {
                    if let Some(a) = act {
                        let _ = write!(fk, " ON {when} {}", a.to_ascii_uppercase());
                    }
                }
                lines.push(fk);
            }
            let _ = writeln!(
                out,
                "CREATE TABLE {} (\n{}\n);",
                self.qt(&t.name),
                lines.join(",\n")
            );
            for idx in &t.indexes {
                // ponytail: MySQL has no partial/expression index form we emit —
                // skip (never a silent plain index); Caps::expr_indexes=false makes
                // the registry report the drop.
                if (idx.expr.is_some() || idx.predicate.is_some()) && self.dialect == Dialect::MySql
                {
                    continue;
                }
                // key: quoted columns, or the canonical SQL key list (expressions
                // already parenthesised by the IR). A key-less index (DBML
                // `` (`WHERE …`) ``) has no SQL form: skip it, fidelity counts it.
                let target = match (&idx.expr, idx.columns.is_empty()) {
                    (Some(e), _) => e.clone(),
                    (None, false) => self.qs(&idx.columns),
                    (None, true) => continue,
                };
                let pred = idx
                    .predicate
                    .as_ref()
                    .map_or(String::new(), |p| format!(" WHERE {p}"));
                let kw = if idx.unique {
                    "CREATE UNIQUE INDEX"
                } else {
                    "CREATE INDEX"
                };
                let name = idx.name.clone().unwrap_or_else(|| {
                    let base: String = format!(
                        "idx_{}_{}",
                        t.name,
                        idx.columns.first().map_or("expr", String::as_str)
                    )
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                    .collect();
                    let mut name = base.clone();
                    let mut n = 1;
                    while used_index_names.contains(&name) {
                        n += 1;
                        name = format!("{base}_{n}");
                    }
                    used_index_names.insert(name.clone());
                    name
                });
                let _ = writeln!(
                    out,
                    "{} {} ON {} ({}){};",
                    kw,
                    self.q(&name),
                    self.qt(&t.name),
                    target,
                    pred
                );
            }
            let _ = writeln!(out);
        }
        Ok(out)
    }
}

fn ident_str(i: &sqlparser::ast::Ident) -> String {
    i.value.clone()
}

fn part_str(p: &sqlparser::ast::ObjectNamePart) -> String {
    p.as_ident().map(ident_str).unwrap_or_else(|| p.to_string())
}

/// Words that cannot be a bare identifier in at least one emitted dialect:
/// PostgreSQL's reserved keywords plus common MySQL/SQLite ones (`key`, `index`).
/// Names like `user` or `order` are common table names, so they get quoted.
const RESERVED: &[&str] = &[
    "all",
    "alter",
    "analyse",
    "analyze",
    "and",
    "any",
    "array",
    "as",
    "asc",
    "asymmetric",
    "authorization",
    "between",
    "binary",
    "both",
    "by",
    "case",
    "cast",
    "check",
    "collate",
    "collation",
    "column",
    "concurrently",
    "constraint",
    "create",
    "cross",
    "current_catalog",
    "current_date",
    "current_role",
    "current_schema",
    "current_time",
    "current_timestamp",
    "current_user",
    "default",
    "deferrable",
    "delete",
    "desc",
    "distinct",
    "do",
    "drop",
    "else",
    "end",
    "except",
    "exists",
    "false",
    "fetch",
    "for",
    "foreign",
    "freeze",
    "from",
    "full",
    "grant",
    "group",
    "having",
    "if",
    "ilike",
    "in",
    "index",
    "initially",
    "inner",
    "insert",
    "intersect",
    "into",
    "is",
    "isnull",
    "join",
    "key",
    "lateral",
    "leading",
    "left",
    "like",
    "limit",
    "localtime",
    "localtimestamp",
    "natural",
    "not",
    "notnull",
    "null",
    "offset",
    "on",
    "only",
    "or",
    "order",
    "outer",
    "overlaps",
    "placing",
    "primary",
    "references",
    "returning",
    "right",
    "select",
    "session_user",
    "set",
    "similar",
    "some",
    "symmetric",
    "system_user",
    "table",
    "tablesample",
    "then",
    "to",
    "trailing",
    "true",
    "union",
    "unique",
    "update",
    "user",
    "using",
    "values",
    "variadic",
    "verbose",
    "when",
    "where",
    "window",
    "with",
];

fn is_reserved(ident: &str) -> bool {
    RESERVED.contains(&ident.to_ascii_lowercase().as_str())
}

/// One `CREATE INDEX` key element from the AST: column or expression, then
/// `[opclass] [ASC|DESC] [NULLS …]` in SQL order (sqlparser's Display puts the
/// operator class after the ordering, which is not valid SQL).
fn index_key_part(ic: &sqlparser::ast::IndexColumn) -> KeyPart {
    let mut suffix: Vec<String> = Vec::new();
    if let Some(oc) = &ic.operator_class {
        suffix.push(oc.to_string());
    }
    let order = ic.column.options.to_string();
    if !order.trim().is_empty() {
        suffix.push(order.trim().to_string());
    }
    let suffix = (!suffix.is_empty()).then(|| suffix.join(" "));
    match &ic.column.expr {
        Expr::Identifier(id) => KeyPart::Column(id.value.clone(), suffix),
        Expr::Nested(e) => KeyPart::Expr(e.to_string(), suffix),
        other => KeyPart::Expr(other.to_string(), suffix),
    }
}

/// Full dotted name: `billing.tenants` keeps its schema, so refs to it still
/// match the table after a round-trip.
fn object_name_to_string(o: &ObjectName) -> String {
    o.0.iter().map(part_str).collect::<Vec<_>>().join(".")
}

fn render_type(dt: &DataType) -> String {
    match dt {
        DataType::Integer(_) | DataType::Int(_) => "INTEGER".into(),
        DataType::BigInt(_) => "BIGINT".into(),
        DataType::Text | DataType::String(_) => "TEXT".into(),
        DataType::Varchar(n) => match n {
            Some(l) => format!("VARCHAR({})", l),
            None => "VARCHAR".into(),
        },
        DataType::Boolean => "BOOLEAN".into(),
        DataType::Timestamp(_, tz) => match tz {
            TimezoneInfo::Tz => "TIMESTAMPTZ".into(),
            _ => "TIMESTAMP".into(),
        },
        DataType::Real | DataType::Float(_) => "REAL".into(),
        DataType::Double(_) => "DOUBLE PRECISION".into(),
        other => other.to_string().to_uppercase(),
    }
}

/// sqlparser's referential action, normalized to the IR vocabulary.
fn action(a: &Option<sqlparser::ast::ReferentialAction>) -> Option<String> {
    a.as_ref().and_then(|a| normalize_action(&a.to_string()))
}
