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

/// Issue #25 snippet: the scan reads 2 columns from TEXT, whatever the parser does.
#[test]
fn issue_25_snippet_totals_come_from_text() {
    let src = "Table users { id integer [pk]\nname varchar\n}";
    assert_eq!(scan("dbml", src), c(1, 2, 0, 0));
    let f = Registry::with_defaults()
        .convert_explicit(src, &id("dbml"), &id("sql"))
        .unwrap()
        .fidelity;
    assert_eq!(
        f.columns.total, 2,
        "total must be the source-declared count"
    );
}

/// Property: a parse that lost entities must NOT report lossless. Fails if
/// fidelity ever goes back to measuring only the post-parse IR.
#[test]
fn doctored_ir_is_reported_as_parser_loss() {
    let reg = Registry::with_defaults();
    let src = "Table a {\n  id int [pk]\n  b int\n}\nTable c {\n  id int\n}\n";
    let mut ir = reg.parse(src, &id("dbml")).unwrap();
    assert!(
        reg.measure(src, &id("dbml"), &id("sql"), ir.clone())
            .unwrap()
            .fidelity
            .idempotent
    );
    ir.tables[0].columns.pop(); // simulate a parser that silently drops `b`
    ir.tables.pop(); // …and table `c` with its column
    let f = reg
        .measure(src, &id("dbml"), &id("sql"), ir)
        .unwrap()
        .fidelity;
    assert!(!f.idempotent, "parse loss must clear the lossless verdict");
    assert!(!f.columns.lossless() && !f.tables.lossless());
    assert_eq!((f.tables.kept, f.tables.total), (1, 2));
    assert_eq!((f.columns.kept, f.columns.total), (1, 3));
    assert!(
        f.dropped
            .iter()
            .any(|d| d.contains("2 column(s) declared in source but missing after parse")),
        "{:?}",
        f.dropped
    );
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
