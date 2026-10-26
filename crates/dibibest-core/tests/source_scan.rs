use dibibest_core::{FormatId, Registry, SourceCounts};
use std::fs;
use std::path::Path;

fn id(s: &str) -> FormatId {
    FormatId::new(s)
}

fn scan(fmt: &str, src: &str) -> SourceCounts {
    Registry::with_defaults()
        .codec(&id(fmt))
        .unwrap()
        .scan(src)
        .unwrap()
}

fn c(tables: usize, columns: usize, refs: usize, indexes: usize) -> SourceCounts {
    SourceCounts {
        tables,
        columns,
        refs,
        indexes,
    }
}

const DBML: &str = "Table users {\n  id int [pk]\n  name varchar [note: 'ref: not a ref']\n}\n\
Table posts {\n  id int [pk]\n  user_id int [ref: > users.id]\n  indexes {\n    user_id\n    (id, user_id) [unique]\n  }\n}\n\
Ref: posts.id > users.id\n";

const SQL: &str = "-- CREATE TABLE ghost (x int);\n\
CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, UNIQUE (name));\n\
CREATE TABLE posts (id INTEGER, user_id INTEGER REFERENCES users(id), \
CONSTRAINT fk FOREIGN KEY (id) REFERENCES users(id));\n\
CREATE UNIQUE INDEX i1 ON posts (user_id);\n";

#[test]
fn dbml_counts() {
    assert_eq!(scan("dbml", DBML), c(2, 4, 2, 2));
}

#[test]
fn sql_counts_all_dialects() {
    for f in ["sql", "sql:mysql", "sql:sqlite"] {
        assert_eq!(scan(f, SQL), c(2, 4, 2, 1), "{f}");
    }
}

#[test]
fn prisma_counts() {
    let src = "datasource db {\n  provider = \"postgresql\"\n}\n\
model User {\n  id Int @id\n  name String?\n  posts Post[]\n}\n\
model Post {\n  id Int @id\n  authorId Int\n  author User @relation(fields: [authorId], references: [id])\n  @@index([authorId])\n}\n";
    assert_eq!(scan("prisma", src), c(2, 4, 1, 1));
}

#[test]
fn erdjson_counts() {
    let src = r#"{"version":1,"nodes":[{"id":"a","name":"a","columns":[{"name":"x"},{"name":"y"}],"indexes":[{}]},{"id":"b","name":"b","columns":[{"name":"z"}]}],"edges":[{}]}"#;
    assert_eq!(scan("erdjson", src), c(2, 3, 1, 1));
}

/// The scan may under-count but must never over-count a correct parse.
#[test]
fn scan_never_exceeds_ir_on_corpus() {
    let reg = Registry::with_defaults();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let src = fs::read_to_string(&p).unwrap();
        let ir = reg.parse(&src, &id("dbml")).unwrap();
        let s = scan("dbml", &src);
        let cols: usize = ir.tables.iter().map(|t| t.columns.len()).sum();
        let idx: usize = ir.tables.iter().map(|t| t.indexes.len()).sum();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(s.tables <= ir.tables.len(), "{name} tables {s:?}");
        assert!(s.columns <= cols, "{name} columns {s:?} vs {cols}");
        assert!(
            s.refs <= ir.refs.len(),
            "{name} refs {s:?} vs {}",
            ir.refs.len()
        );
        assert!(s.indexes <= idx, "{name} indexes {s:?} vs {idx}");
    }
}
