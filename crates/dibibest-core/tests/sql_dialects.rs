//! SQL dialect conformance: per-dialect round-trips (PG enums, MySQL
//! AUTO_INCREMENT, SQLite AUTOINCREMENT) and the enum-drop fidelity report.

use dibibest_core::{FormatId, Registry};

fn id(s: &str) -> FormatId {
    FormatId::new(s)
}

const DBML: &str = "Enum role {\n  admin\n  user\n}\n\
Table users {\n  id int [pk, increment]\n  name varchar(80) [not null]\n  role role\n}\n\
Table posts {\n  id int [pk, increment]\n  user_id int\n}\n\
Ref: posts.user_id > users.id\n";

/// emit(parse(src)) → parse must equal parse(src), in the given dialect.
fn assert_roundtrip(dialect: &str, src: &str) -> String {
    let reg = Registry::with_defaults();
    let ir = reg.parse(src, &id(dialect)).unwrap();
    let out = reg.emit(&ir, &id(dialect)).unwrap();
    assert_eq!(
        reg.parse(&out, &id(dialect)).unwrap(),
        ir,
        "{dialect}:\n{out}"
    );
    out
}

#[test]
fn dialect_ids_are_registered() {
    let ids = Registry::with_defaults().ids();
    for want in ["sql", "sql:mysql", "sql:sqlite"] {
        assert!(ids.iter().any(|i| i == want), "{want} missing in {ids:?}");
    }
}

#[test]
fn pg_enum_roundtrip() {
    let out = assert_roundtrip(
        "sql",
        "CREATE TYPE role AS ENUM ('admin', 'it''s');\n\
         CREATE TABLE u (id INTEGER PRIMARY KEY, r role);\n",
    );
    assert!(out.contains("CREATE TYPE role AS ENUM ('admin', 'it''s');"));
    let ir = Registry::with_defaults().parse(&out, &id("sql")).unwrap();
    assert_eq!(ir.enums[0].values, ["admin", "it's"]);
}

#[test]
fn mysql_auto_increment_roundtrip() {
    let out = assert_roundtrip(
        "sql:mysql",
        "CREATE TABLE `users` (`id` INTEGER PRIMARY KEY AUTO_INCREMENT, `n` TEXT);\n",
    );
    assert!(
        out.contains("`id` INTEGER PRIMARY KEY AUTO_INCREMENT"),
        "{out}"
    );
    let ir = Registry::with_defaults()
        .parse(&out, &id("sql:mysql"))
        .unwrap();
    assert!(ir.tables[0].columns[0].autoincrement);
    assert_eq!(
        ir.tables[0].columns[0].name, "id",
        "backticks must not leak into names"
    );
}

#[test]
fn sqlite_autoincrement_roundtrip() {
    let out = assert_roundtrip(
        "sql:sqlite",
        "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, n TEXT);\n",
    );
    assert!(
        out.contains("id INTEGER PRIMARY KEY AUTOINCREMENT"),
        "{out}"
    );
    assert!(!out.contains("AUTO_INCREMENT"));
}

#[test]
fn dbml_to_each_dialect_keeps_structure() {
    let reg = Registry::with_defaults();
    for (d, needle) in [
        ("sql", "CREATE TYPE role"),
        ("sql:mysql", "AUTO_INCREMENT"),
        ("sql:sqlite", "AUTOINCREMENT"),
    ] {
        let c = reg.convert_explicit(DBML, &id("dbml"), &id(d)).unwrap();
        assert!(c.output.contains(needle), "{d}:\n{}", c.output);
        assert!(
            c.fidelity.tables.lossless() && c.fidelity.refs.lossless(),
            "{d}"
        );
    }
}

#[test]
fn enums_drop_with_report_on_mysql_and_sqlite_only() {
    let reg = Registry::with_defaults();
    for d in ["sql:mysql", "sql:sqlite"] {
        let c = reg.convert_explicit(DBML, &id("dbml"), &id(d)).unwrap();
        assert!(!c.output.contains("CREATE TYPE"), "{d}");
        assert!(
            c.fidelity.dropped.iter().any(|g| g.contains("enums")),
            "{d} must report the enum drop, got {:?}",
            c.fidelity.dropped
        );
    }
    let pg = reg.convert_explicit(DBML, &id("dbml"), &id("sql")).unwrap();
    assert!(pg.fidelity.dropped.is_empty(), "{:?}", pg.fidelity.dropped);
}

#[test]
fn mysql_quoted_identifiers_roundtrip() {
    let out = assert_roundtrip(
        "sql:mysql",
        "CREATE TABLE `order` (`my col` INTEGER PRIMARY KEY, `we``ird` TEXT);\n\
         CREATE INDEX `ix` ON `order` (`we``ird`);\n",
    );
    assert!(out.contains("`my col`"), "{out}");
}

const PARTIAL: &str = "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT);\n\
                       CREATE INDEX ix ON t (a) WHERE a IS NOT NULL;\n";

#[test]
fn partial_index_kept_on_pg_sqlite_dropped_on_mysql() {
    let reg = Registry::with_defaults();
    for d in ["sql", "sql:sqlite"] {
        let out = assert_roundtrip(d, PARTIAL);
        assert!(out.contains("WHERE a IS NOT NULL"), "{d}: {out}");
        let c = reg.convert_explicit(PARTIAL, &id("sql"), &id(d)).unwrap();
        assert!(c.fidelity.indexes.lossless(), "{d}");
    }
    let c = reg
        .convert_explicit(PARTIAL, &id("sql"), &id("sql:mysql"))
        .unwrap();
    assert!(!c.output.contains("INDEX"), "{}", c.output);
    assert!(
        !c.fidelity.indexes.lossless(),
        "mysql must report index drop"
    );
    assert!(c.fidelity.dropped.iter().any(|g| g.contains("partial")));
}

#[test]
fn fk_without_referenced_columns_never_emits_empty_parens() {
    let reg = Registry::with_defaults();
    let mut ir = reg.parse(DBML, &id("dbml")).unwrap();
    ir.refs[0].to_cols.clear();

    // target PK is known: defaults to it
    let out = reg.emit(&ir, &id("sql")).unwrap();
    assert!(!out.contains("()"), "{out}");
    assert!(out.contains("REFERENCES users(id)"), "{out}");

    // target PK unknown: FK skipped, never invalid SQL
    let users = ir.tables.iter_mut().find(|t| t.name == "users").unwrap();
    for c in &mut users.columns {
        c.pk = false;
    }
    let out = reg.emit(&ir, &id("sql")).unwrap();
    assert!(!out.contains("()") && !out.contains("FOREIGN KEY"), "{out}");
    assert!(reg.parse(&out, &id("sql")).unwrap().refs.is_empty());
}

#[test]
fn mysql_quotes_schema_qualified_names_per_part() {
    let src = "Table billing.plans {\n  id int [pk]\n}\nTable billing.subs {\n  id int [pk]\n  \
               plan_id int [ref: > billing.plans.id]\n}\n";
    let conv = dibibest_core::Registry::with_defaults()
        .convert_explicit(
            src,
            &dibibest_core::FormatId::new("dbml"),
            &dibibest_core::FormatId::new("sql:mysql"),
        )
        .unwrap();
    assert!(
        conv.output.contains("CREATE TABLE `billing`.`plans` ("),
        "{}",
        conv.output
    );
    assert!(
        conv.output.contains("REFERENCES `billing`.`plans`(`id`)"),
        "{}",
        conv.output
    );
    assert_eq!((conv.fidelity.refs.kept, conv.fidelity.refs.total), (1, 1));
}

#[test]
fn dotted_column_name_stays_one_identifier() {
    // A dot inside a column name is not a schema separator (CodeCora finding on #48).
    let src = "Table t {\n  id int [pk]\n  \"v1.2\" int\n}\n";
    let reg = dibibest_core::Registry::with_defaults();
    for (to, want) in [
        ("sql", "\"v1.2\" int"),
        ("sql:sqlite", "\"v1.2\" int"),
        ("sql:mysql", "`v1.2` int"),
    ] {
        let id = dibibest_core::FormatId::new(to);
        let conv = reg
            .convert_explicit(src, &dibibest_core::FormatId::new("dbml"), &id)
            .unwrap();
        assert!(conv.output.contains(want), "{to}:\n{}", conv.output);
        let back = reg.parse(&conv.output, &id).unwrap();
        assert!(
            back.tables[0].columns.iter().any(|c| c.name == "v1.2"),
            "{to}"
        );
        assert_eq!(conv.fidelity.columns.kept, 2, "{to}");
    }
}

#[test]
fn quotes_inside_a_name_cannot_break_out() {
    // Cora finding on #48: a name that starts and ends with `"` used to pass
    // through raw, so `"x" int, "y"` became two column definitions.
    let src = "CREATE TABLE t (id int PRIMARY KEY, \"\"\"x\"\" int, \"\"y\"\"\" int);";
    let reg = dibibest_core::Registry::with_defaults();
    for to in ["sql", "sql:sqlite", "sql:mysql"] {
        let id = dibibest_core::FormatId::new(to);
        let conv = reg
            .convert_explicit(src, &dibibest_core::FormatId::new("sql"), &id)
            .unwrap();
        let back = reg.parse(&conv.output, &id).unwrap();
        let names: Vec<&str> = back.tables[0]
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, ["id", "\"x\" int, \"y\""], "{to}:\n{}", conv.output);
    }
}

#[test]
fn dbml_index_columns_are_unquoted_identifiers() {
    let src = "Table \"User Accounts\" {\n  ID int [pk]\n  \"e-mail\" varchar\n\n  \
               indexes {\n    (\"e-mail\", ID) [name: 'ua']\n  }\n}\n";
    let reg = dibibest_core::Registry::with_defaults();
    let ir = reg
        .parse(src, &dibibest_core::FormatId::new("dbml"))
        .unwrap();
    assert_eq!(ir.tables[0].indexes[0].columns, ["e-mail", "ID"]);
    let mysql = reg
        .convert_explicit(
            src,
            &dibibest_core::FormatId::new("dbml"),
            &dibibest_core::FormatId::new("sql:mysql"),
        )
        .unwrap()
        .output;
    assert!(mysql.contains("(`e-mail`, `ID`)"), "{mysql}");
}

#[test]
fn index_operator_class_and_ordering_round_trip() {
    let src = "CREATE TABLE t (id int PRIMARY KEY, x text);\n\
               CREATE INDEX ix ON t (x text_pattern_ops DESC);\n\
               CREATE INDEX iy ON t (lower(x) DESC NULLS LAST);\n";
    let reg = Registry::with_defaults();
    for to in ["sql", "sql:sqlite", "dbml"] {
        let c = reg.convert_explicit(src, &id("sql"), &id(to)).unwrap();
        assert_eq!(
            (c.fidelity.indexes.kept, c.fidelity.indexes.total),
            (2, 2),
            "{to}:\n{}",
            c.output
        );
    }
    let sql = reg
        .convert_explicit(src, &id("sql"), &id("sql"))
        .unwrap()
        .output;
    assert!(sql.contains("ON t (x text_pattern_ops DESC);"), "{sql}");
    assert!(sql.contains("ON t ((lower(x)) DESC NULLS LAST);"), "{sql}");
    // through DBML and back keeps the operator class and the ordering
    let dbml = reg
        .convert_explicit(src, &id("sql"), &id("dbml"))
        .unwrap()
        .output;
    let back = reg
        .convert_explicit(&dbml, &id("dbml"), &id("sql"))
        .unwrap()
        .output;
    assert!(back.contains("ON t (x text_pattern_ops DESC);"), "{back}");
}

#[test]
fn unnamed_expression_indexes_get_distinct_names() {
    let src = "Table t {\n  id int [pk]\n  a int\n  b int\n\n  indexes {\n    \
               (`lower(a::text)`)\n    (`b * 2`)\n  }\n}\n";
    let reg = Registry::with_defaults();
    let c = reg.convert_explicit(src, &id("dbml"), &id("sql")).unwrap();
    assert!(
        c.output.contains("CREATE INDEX idx_t_expr ON t"),
        "{}",
        c.output
    );
    assert!(
        c.output.contains("CREATE INDEX idx_t_expr_2 ON t"),
        "{}",
        c.output
    );
    assert_eq!((c.fidelity.indexes.kept, c.fidelity.indexes.total), (2, 2));
}
