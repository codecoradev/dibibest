//! Round-trip conformance tests — the "bolak-balik" promise, enforced.

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
