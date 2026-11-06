//! Round-trip conformance tests — the round-trip promise, enforced.

use dibibest_core::{detect_format, FormatId, Registry};

const SHOP_DBML: &str = r#"
Table users {
  id integer [pk]
  email varchar [not null, unique]
  created_at datetime
}

Table products {
  id integer [pk]
  name varchar [not null]
  price_cents integer [default: 0]
}

Table orders {
  id integer [pk]
  user_id integer [not null]
  status varchar
  total_cents integer [default: 0]

  indexes {
    (user_id, created_at) [name: 'idx_orders_user_created']
    (`WHERE status = 'open'`) [unique, name: 'idx_open_orders']
  }
}

Ref: orders.user_id > users.id
"#;

const SHOP_SQL: &str = r#"
CREATE TABLE users (
  id INTEGER PRIMARY KEY,
  email VARCHAR NOT NULL,
  created_at TIMESTAMP
);
CREATE TABLE orders (
  id INTEGER PRIMARY KEY,
  user_id INTEGER NOT NULL,
  status VARCHAR,
  total_cents INTEGER DEFAULT 0,
  FOREIGN KEY (user_id) REFERENCES users(id)
);
CREATE INDEX idx_orders_user ON orders (user_id);
"#;

#[test]
fn dbml_round_trip_is_idempotent() {
    let reg = Registry::with_defaults();
    let ir1 = reg
        .codec(&FormatId::new("dbml"))
        .and_then(|c| c.parse(SHOP_DBML))
        .expect("parse dbml");
    assert_eq!(ir1.tables.len(), 3);
    assert_eq!(ir1.refs.len(), 1);
    let orders = ir1.table("orders").expect("orders table");
    assert_eq!(orders.indexes.len(), 2);

    let emitted = reg
        .codec(&FormatId::new("dbml"))
        .unwrap()
        .emit(&ir1)
        .expect("emit dbml");
    let ir2 = reg
        .codec(&FormatId::new("dbml"))
        .unwrap()
        .parse(&emitted)
        .expect("re-parse emitted dbml");
    assert_eq!(ir1, ir2, "DBML round-trip must be lossless");
}

#[test]
fn dbml_to_sql_reports_fidelity() {
    let reg = Registry::with_defaults();
    let c = reg
        .convert_explicit(SHOP_DBML, &FormatId::new("dbml"), &FormatId::new("sql"))
        .expect("convert dbml->sql");
    assert!(c.output.contains("CREATE TABLE users"));
    assert!(c.output.contains("FOREIGN KEY (user_id) REFERENCES users"));
    // fidelity: everything survives SQL round-trip in this subset
    assert!(c.fidelity.tables.lossless());
    assert!(c.fidelity.columns.lossless());
    assert!(c.fidelity.refs.lossless());
    assert!(c.fidelity.idempotent, "source-format idempotency must hold");
}

#[test]
fn sql_to_dbml_and_back_stays_equivalent() {
    let reg = Registry::with_defaults();
    let c1 = reg
        .convert_explicit(SHOP_SQL, &FormatId::new("sql"), &FormatId::new("dbml"))
        .expect("sql->dbml");
    assert!(c1.fidelity.lossless_counts(), "sql->dbml must be lossless");
    let c2 = reg
        .convert_explicit(&c1.output, &FormatId::new("dbml"), &FormatId::new("sql"))
        .expect("dbml->sql");
    assert!(
        c2.fidelity.lossless_counts(),
        "back to sql must be lossless"
    );
}

#[test]
fn detect_format_prefers_dbml_and_sql() {
    assert_eq!(detect_format(SHOP_DBML), Some("dbml"));
    assert_eq!(detect_format(SHOP_SQL), Some("sql"));
    assert_eq!(detect_format("hello world"), None);
}

#[test]
fn unknown_format_is_a_clean_error() {
    let reg = Registry::with_defaults();
    let err = reg
        .convert_explicit(SHOP_DBML, &FormatId::new("erd-json"), &FormatId::new("sql"))
        .unwrap_err();
    assert!(err.to_string().contains("unknown format"));
}

/// Helper for readable assertions on Fidelity.
trait LosslessCounts {
    fn lossless_counts(&self) -> bool;
}
impl LosslessCounts for dibibest_core::Fidelity {
    fn lossless_counts(&self) -> bool {
        self.tables.lossless()
            && self.columns.lossless()
            && self.refs.lossless()
            && self.indexes.lossless()
    }
}

#[test]
fn one_line_blocks_parse_and_round_trip() {
    let reg = Registry::with_defaults();
    let src =
        "Table a { id integer [pk] }\n\nTable b { code varchar [not null] }\nRef: b.id > a.id\n";
    let c = reg
        .convert_explicit(src, &FormatId::new("dbml"), &FormatId::new("sql"))
        .expect("one-liner convert");
    assert!(c.output.contains("CREATE TABLE a"));
    assert!(c.output.contains("CREATE TABLE b"));
}

fn dbml_to_sql(src: &str) -> String {
    Registry::with_defaults()
        .convert_explicit(src, &FormatId::new("dbml"), &FormatId::new("sql"))
        .expect("convert")
        .output
}

#[test]
fn block_borders_are_line_agnostic() {
    // Bug A: inline column after `{`, rest on following lines.
    let sql = dbml_to_sql("Table users { id integer [pk]\n  name varchar\n}\n");
    assert!(
        sql.contains("id integer") && sql.contains("name varchar"),
        "{sql}"
    );

    // Bug B: last column shares the line with the closing `}`.
    let sql = dbml_to_sql("Table users {\n  id integer [pk]\n  name varchar [not null] }\n");
    assert!(
        sql.contains("id integer") && sql.contains("name varchar NOT NULL"),
        "{sql}"
    );

    // One-liner still works.
    assert!(dbml_to_sql("Table a { id int [pk] }\n").contains("CREATE TABLE a"));

    // Ref and Enum with content split from the braces.
    let sql =
        dbml_to_sql("Table a { id int [pk] }\nTable b { a_id int }\nRef r { b.a_id > a.id\n}\n");
    assert!(sql.contains("FOREIGN KEY"), "{sql}");
    let dbml = "Enum st { open\n  closed }\nTable t { s st }\n";
    let reg = Registry::with_defaults();
    let d = FormatId::new("dbml");
    let once = reg.convert_explicit(dbml, &d, &d).unwrap().output;
    assert!(once.contains("open") && once.contains("closed"), "{once}");
    // idempotency: parse(emit(parse(x))) == parse(x)
    let twice = reg.convert_explicit(&once, &d, &d).unwrap().output;
    assert_eq!(once, twice);
}

#[test]
fn block_border_error_reports_physical_line() {
    let err = Registry::with_defaults()
        .convert_explicit(
            "Table t {\n  id int\n  !!! }\n",
            &FormatId::new("dbml"),
            &FormatId::new("sql"),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("line 3"), "{err}");
}

/// dbdiagram.io's default sample: `<` (one-to-many) refs put the FK on the
/// right-hand ("many") table. Regression for #42.
const DBDIAGRAM_SAMPLE: &str = r#"
Table follows {
  following_user_id integer
  followed_user_id integer
  created_at timestamp
}

Table users {
  id integer [primary key]
  username varchar
}

Table posts {
  id integer [primary key]
  user_id integer [not null]
}

Ref user_posts: posts.user_id > users.id // many-to-one

Ref: users.id < follows.following_user_id

Ref: users.id < follows.followed_user_id
"#;

#[test]
fn one_to_many_ref_puts_fk_on_many_side_in_sql() {
    let reg = Registry::with_defaults();
    let conv = reg
        .convert_explicit(
            DBDIAGRAM_SAMPLE,
            &FormatId::new("dbml"),
            &FormatId::new("sql"),
        )
        .unwrap();
    let sql = &conv.output;
    let block = |t: &str| {
        let start = sql.find(&format!("CREATE TABLE {t} (")).unwrap();
        let end = start + sql[start..].find(");").unwrap();
        sql[start..end].to_string()
    };
    assert!(
        !block("users").contains("FOREIGN KEY"),
        "users must not carry FKs:\n{sql}"
    );
    let follows = block("follows");
    assert!(follows.contains("FOREIGN KEY (following_user_id) REFERENCES users(id)"));
    assert!(follows.contains("FOREIGN KEY (followed_user_id) REFERENCES users(id)"));
    assert!(block("posts").contains("FOREIGN KEY (user_id) REFERENCES users(id)"));
    assert_eq!(conv.fidelity.refs.kept, 3);
    assert_eq!(conv.fidelity.refs.total, 3);
}

/// Referential actions survive every target format and back. Regression for #43.
const CASCADE_DBML: &str = r#"
Table users {
  id int [pk]
}

Table orders {
  id int [pk]
  user_id int
}

Ref: orders.user_id > users.id [delete: cascade, update: set null]
"#;

#[test]
fn referential_actions_survive_every_target() {
    let reg = Registry::with_defaults();
    for to in [
        "dbml",
        "sql",
        "sql:mysql",
        "sql:sqlite",
        "prisma",
        "erdjson",
    ] {
        let conv = reg
            .convert_explicit(CASCADE_DBML, &FormatId::new("dbml"), &FormatId::new(to))
            .unwrap();
        let back = reg.parse(&conv.output, &FormatId::new(to)).unwrap();
        let r = &back.refs[0];
        assert_eq!(
            r.on_delete.as_deref(),
            Some("cascade"),
            "{to}:\n{}",
            conv.output
        );
        assert_eq!(
            r.on_update.as_deref(),
            Some("set null"),
            "{to}:\n{}",
            conv.output
        );
        assert_eq!(
            (conv.fidelity.refs.kept, conv.fidelity.refs.total),
            (1, 1),
            "{to}"
        );
        assert!(conv.fidelity.idempotent, "{to}");
    }
}

#[test]
fn sql_on_delete_reaches_dbml() {
    let sql = "CREATE TABLE users (id int PRIMARY KEY);\n\
               CREATE TABLE orders (id int PRIMARY KEY, \
               user_id int REFERENCES users(id) ON DELETE CASCADE);";
    let conv = Registry::with_defaults()
        .convert_explicit(sql, &FormatId::new("sql"), &FormatId::new("dbml"))
        .unwrap();
    assert!(
        conv.output
            .contains("Ref: orders.user_id > users.id [delete: cascade]"),
        "{}",
        conv.output
    );
}

/// Default literal kinds and non-plain identifiers survive DBML → SQL and
/// back. Regression for #45.
const DEFAULTS_DBML: &str = r#"
Table "order items" {
  "line no" int [pk]
  plan varchar [not null, default: 'free']
  label varchar [default: 'a, b, c']
  quote varchar [default: 'it\'s']
  qty int [default: 0]
  active boolean [default: true]
  meta jsonb [default: `'{}'::jsonb`]
  created_at timestamptz [default: `now()`]
}
"#;

#[test]
fn defaults_and_identifiers_emit_valid_sql() {
    let reg = Registry::with_defaults();
    let sql = reg
        .convert_explicit(DEFAULTS_DBML, &FormatId::new("dbml"), &FormatId::new("sql"))
        .unwrap()
        .output;
    for want in [
        "CREATE TABLE \"order items\" (",
        "\"line no\" int PRIMARY KEY",
        "DEFAULT 'free'",
        "DEFAULT 'a, b, c'",
        "DEFAULT 'it''s'",
        "DEFAULT 0",
        "DEFAULT true",
        "DEFAULT '{}'::jsonb",
        "DEFAULT now()",
    ] {
        assert!(sql.contains(want), "missing `{want}` in:\n{sql}");
    }
    let back = reg
        .convert_explicit(&sql, &FormatId::new("sql"), &FormatId::new("dbml"))
        .unwrap()
        .output;
    for want in [
        "default: 'free'",
        "default: 'a, b, c'",
        "default: 'it\\'s'",
        "default: 0",
        "default: true",
        "default: `'{}'::JSONB`", // sqlparser upper-cases cast types
        "default: `now()`",
    ] {
        assert!(back.contains(want), "missing `{want}` in:\n{back}");
    }
}

#[test]
fn string_and_enum_defaults_reach_prisma() {
    let src = "Enum role {\n  user\n  admin\n}\nTable users {\n  id int [pk]\n  \
               plan varchar [default: 'free']\n  role role [default: 'user']\n}\n";
    let reg = Registry::with_defaults();
    let prisma = reg
        .convert_explicit(src, &FormatId::new("dbml"), &FormatId::new("prisma"))
        .unwrap();
    assert!(
        prisma.output.contains("@default(\"free\")"),
        "{}",
        prisma.output
    );
    assert!(
        prisma.output.contains("@default(user)"),
        "{}",
        prisma.output
    );
    let ir = reg.parse(&prisma.output, &FormatId::new("prisma")).unwrap();
    let col = |n: &str| {
        ir.tables[0]
            .columns
            .iter()
            .find(|c| c.name == n)
            .and_then(|c| c.default.clone())
    };
    assert_eq!(col("plan").as_deref(), Some("'free'"));
    assert_eq!(col("role").as_deref(), Some("'user'"));
}

#[test]
fn reserved_word_identifiers_are_quoted() {
    let src = "Table user {\n  id int [pk]\n}\nTable order {\n  id int [pk]\n  \
               user_id int [ref: > user.id]\n  \"group\" varchar\n}\n";
    let conv = Registry::with_defaults()
        .convert_explicit(src, &FormatId::new("dbml"), &FormatId::new("sql"))
        .unwrap();
    for want in [
        "CREATE TABLE \"user\" (",
        "CREATE TABLE \"order\" (",
        "\"group\" varchar",
        "REFERENCES \"user\"(id)",
    ] {
        assert!(
            conv.output.contains(want),
            "missing `{want}` in:\n{}",
            conv.output
        );
    }
    // plain names stay bare
    assert!(conv.output.contains("  user_id int,"), "{}", conv.output);
    assert_eq!((conv.fidelity.refs.kept, conv.fidelity.refs.total), (1, 1));
}

/// Expression, ordered and partial indexes produce valid SQL and come back to
/// the same DBML. Regression for #49 (DBML backticks leaked into SQL).
const EXPR_INDEX_DBML: &str = r#"
Table alerts {
  id int [pk]
  email varchar
  amount int
  fired_at timestamptz
  resolved boolean

  indexes {
    (`lower(email)`) [name: 'by_email']
    (`fired_at DESC`) [name: 'recent']
    (id, `amount DESC`) [name: 'mixed']
    (`coalesce(amount, 0)`, fired_at) [name: 'coalesced']
    (`fired_at WHERE resolved = false`) [unique, name: 'open_alerts']
  }
}
"#;

#[test]
fn expression_and_partial_indexes_emit_valid_sql() {
    let reg = Registry::with_defaults();
    let conv = reg
        .convert_explicit(
            EXPR_INDEX_DBML,
            &FormatId::new("dbml"),
            &FormatId::new("sql"),
        )
        .unwrap();
    let sql = &conv.output;
    for want in [
        "CREATE INDEX by_email ON alerts ((lower(email)));",
        "CREATE INDEX recent ON alerts (fired_at DESC);",
        "CREATE INDEX mixed ON alerts (id, amount DESC);",
        "CREATE INDEX coalesced ON alerts ((coalesce(amount, 0)), fired_at);",
        "CREATE UNIQUE INDEX open_alerts ON alerts (fired_at) WHERE resolved = false;",
    ] {
        assert!(sql.contains(want), "missing `{want}` in:\n{sql}");
    }
    assert!(!sql.contains('`'), "DBML backticks leaked:\n{sql}");
    assert_eq!(
        (conv.fidelity.indexes.kept, conv.fidelity.indexes.total),
        (5, 5)
    );
    // and back: SQL → DBML restores the same index lines
    let back = reg
        .convert_explicit(sql, &FormatId::new("sql"), &FormatId::new("dbml"))
        .unwrap()
        .output;
    for want in [
        "(`lower(email)`) [name: 'by_email']",
        "(`fired_at DESC`) [name: 'recent']",
        "(id, `amount DESC`) [name: 'mixed']",
        "(`coalesce(amount, 0)`, fired_at) [name: 'coalesced']",
        "(`fired_at WHERE resolved = false`) [unique, name: 'open_alerts']",
    ] {
        assert!(back.contains(want), "missing `{want}` in:\n{back}");
    }
}

#[test]
fn keyless_index_is_reported_dropped_for_sql() {
    let src = "Table t {\n  id int [pk]\n  s varchar\n\n  indexes {\n    \
               (`WHERE s = 'open'`) [unique, name: 'only_open']\n  }\n}\n";
    let conv = Registry::with_defaults()
        .convert_explicit(src, &FormatId::new("dbml"), &FormatId::new("sql"))
        .unwrap();
    assert!(!conv.output.contains("only_open"), "{}", conv.output);
    assert_eq!(
        (conv.fidelity.indexes.kept, conv.fidelity.indexes.total),
        (0, 1)
    );
    let note = conv.fidelity.dropped.join(" | ");
    assert!(
        note.contains("without key columns") && note.contains("t.only_open"),
        "{note}"
    );
}
