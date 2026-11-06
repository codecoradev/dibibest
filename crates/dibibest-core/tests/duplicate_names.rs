//! A table, column (within one table) or enum declared twice is refused with a
//! named error in every source format. Before, a repeated column crashed the
//! web canvas (keyed list, 500) and a repeated table was merged into one node
//! while the fidelity report still said lossless.

use dibibest_core::{default_registry, FormatId};

fn err(src: &str, from: &str) -> String {
    match default_registry().parse(src, &FormatId(from.into())) {
        Ok(_) => panic!("{from} accepted a duplicate:\n{src}"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn dbml_duplicate_column() {
    let e = err("Table a {\n  id int\n  id int\n}\n", "dbml");
    assert!(e.contains("duplicate column `a.id`"), "{e}");
}

#[test]
fn dbml_duplicate_table() {
    let e = err("Table a {\n  id int\n}\nTable a {\n  x int\n}\n", "dbml");
    assert!(e.contains("duplicate table `a`"), "{e}");
}

#[test]
fn dbml_duplicate_enum() {
    let e = err("Enum s {\n  a\n}\nEnum s {\n  b\n}\n", "dbml");
    assert!(e.contains("duplicate enum `s`"), "{e}");
}

#[test]
fn sql_duplicates() {
    for d in ["sql", "sql:mysql", "sql:sqlite"] {
        let e = err("CREATE TABLE a (id int, id int);", d);
        assert!(e.contains("duplicate column `a.id`"), "{d}: {e}");
        let e = err("CREATE TABLE a (id int);\nCREATE TABLE a (x int);", d);
        assert!(e.contains("duplicate table `a`"), "{d}: {e}");
    }
}

#[test]
fn prisma_duplicates() {
    let e = err("model A {\n  id Int @id\n  id Int\n}\n", "prisma");
    assert!(e.contains("duplicate column `A.id`"), "{e}");
}

#[test]
fn erdjson_duplicates() {
    let col = r#"{"name":"id","type":"int","isPk":false,"isFk":false,"isUnique":false,"isNullable":true}"#;
    let src = format!(
        r#"{{"version":1,"nodes":[{{"id":"a","name":"a","position":null,"columns":[{col},{col}]}}],"edges":[],"meta":{{"layout":null}}}}"#
    );
    let e = err(&src, "erdjson");
    assert!(e.contains("duplicate column `a.id`"), "{e}");
}

#[test]
fn distinct_names_still_parse() {
    // same column name in different tables, and names that differ only in case
    let src = "Table a {\n  id int\n}\nTable b {\n  id int\n}\nTable A {\n  ID int\n  id int\n}\n";
    let ir = default_registry()
        .parse(src, &FormatId("dbml".into()))
        .unwrap();
    assert_eq!(ir.tables.len(), 3);
}
