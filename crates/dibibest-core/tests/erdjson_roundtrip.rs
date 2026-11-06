//! ERD-JSON codec: snapshot, determinism, idempotent round-trip, layout hook.

use dibibest_core::erdjson::{apply_layout, LayoutStrategy};
use dibibest_core::{FormatId, Registry};
use serde_json::Value;
use std::fs;
use std::path::Path;

const DBML: &str = r#"Enum role {
  user
  admin
}

Table users {
  id int [pk, increment]
  email varchar [unique, not null, note: 'login']
  role role [default: 'user']
}

Table posts {
  id int [pk]
  user_id int [not null]
  indexes {
    user_id [name: 'posts_user']
  }
}

Ref: posts.user_id > users.id
"#;

const SNAPSHOT: &str = include_str!("snapshots/erdjson_basic.json");

fn dbml_to_erd(reg: &Registry, src: &str) -> String {
    reg.convert_explicit(src, &FormatId::new("dbml"), &FormatId::new("erdjson"))
        .unwrap()
        .output
}

#[test]
fn dbml_snapshot() {
    let out = dbml_to_erd(&Registry::with_defaults(), DBML);
    assert_eq!(out, SNAPSHOT, "ERD-JSON snapshot drifted:\n{out}");
}

#[test]
fn emit_is_deterministic() {
    let reg = Registry::with_defaults();
    assert_eq!(dbml_to_erd(&reg, DBML), dbml_to_erd(&reg, DBML));
    assert!(!dbml_to_erd(&reg, DBML).contains("generatedAt"));
}

#[test]
fn roundtrip_idempotent_on_corpus() {
    let reg = Registry::with_defaults();
    let erd = FormatId::new("erdjson");
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut n = 0;
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_none_or(|x| x != "dbml") {
            continue;
        }
        let json1 = dbml_to_erd(&reg, &fs::read_to_string(&p).unwrap());
        let ir = reg.parse(&json1, &erd).unwrap();
        let json2 = reg.emit(&ir, &erd).unwrap();
        assert_eq!(json1, json2, "{} not idempotent", p.display());
        n += 1;
    }
    assert!(n > 0);
}

#[test]
fn layout_survives_roundtrip() {
    let reg = Registry::with_defaults();
    let erd = FormatId::new("erdjson");
    let mut v: Value = serde_json::from_str(&dbml_to_erd(&reg, DBML)).unwrap();
    apply_layout(&mut v, LayoutStrategy::Grid).unwrap();
    let laid = serde_json::to_string_pretty(&v).unwrap() + "\n";
    let again = reg.emit(&reg.parse(&laid, &erd).unwrap(), &erd).unwrap();
    let v2: Value = serde_json::from_str(&again).unwrap();
    assert_eq!(v2["meta"]["layout"], "grid");
    assert_eq!(v2["nodes"][1]["position"], v["nodes"][1]["position"]);
    // and a second pass is byte-stable
    assert_eq!(
        again,
        reg.emit(&reg.parse(&again, &erd).unwrap(), &erd).unwrap()
    );
}

#[test]
fn grid_layout_is_deterministic_row_major() {
    let reg = Registry::with_defaults();
    let src = (0..5)
        .map(|i| format!("Table t{i} {{\n  id int [pk]\n}}\n"))
        .collect::<String>();
    let base: Value = serde_json::from_str(&dbml_to_erd(&reg, &src)).unwrap();
    let (mut a, mut b) = (base.clone(), base);
    apply_layout(&mut a, LayoutStrategy::Grid).unwrap();
    apply_layout(&mut b, LayoutStrategy::Grid).unwrap();
    assert_eq!(a, b);
    // 5 nodes → 3 columns: row 0 = t0..t2, row 1 = t3,t4
    let pos = |i: usize| {
        (
            a["nodes"][i]["position"]["x"].as_i64(),
            a["nodes"][i]["position"]["y"].as_i64(),
        )
    };
    assert_eq!(pos(0), (Some(0), Some(0)));
    assert_eq!(pos(2), (Some(640), Some(0)));
    assert_eq!(pos(3), (Some(0), Some(360)));
    assert_eq!(a["meta"]["layout"], "grid");
}

#[test]
fn parse_rejects_dangling_edge_and_bad_version() {
    let reg = Registry::with_defaults();
    let erd = FormatId::new("erdjson");
    let bad = r#"{"version":1,"nodes":[],"edges":[{"id":"e","from":{"table":"x","columns":[]},"to":{"table":"y","columns":[]},"kind":"many-to-one"}],"meta":{"layout":null}}"#;
    assert!(reg.parse(bad, &erd).is_err());
    let v2 = r#"{"version":2,"nodes":[],"edges":[],"meta":{"layout":null}}"#;
    assert!(reg.parse(v2, &erd).is_err());
}

#[test]
fn unknown_edge_action_is_a_parse_error() {
    // An action the IR cannot represent must fail loudly, never vanish (#43).
    let reg = dibibest_core::Registry::with_defaults();
    let erd = reg
        .convert_explicit(
            "Table a {\n  id int [pk]\n}\nTable b {\n  a_id int\n}\nRef: b.a_id > a.id [delete: cascade]\n",
            &dibibest_core::FormatId::new("dbml"),
            &dibibest_core::FormatId::new("erdjson"),
        )
        .unwrap()
        .output
        .replace("\"cascade\"", "\"explode\"");
    let err = reg
        .parse(&erd, &dibibest_core::FormatId::new("erdjson"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("unknown onDelete `explode`"), "{err}");
}

#[test]
fn grid_row_grows_with_its_tallest_table() {
    // 23 columns is 595px tall: the row below must start under it, not at the
    // fixed 360px cell (that put `events` on top of `activities`)
    let tall = |name: &str, n: usize| {
        let cols: Vec<_> = (0..n)
            .map(|i| serde_json::json!({"name": format!("c{i}"), "type": "int", "isPk": false, "isFk": false, "isUnique": false, "isNullable": true}))
            .collect();
        serde_json::json!({"id": name, "name": name, "position": null, "columns": cols})
    };
    let mut doc = serde_json::json!({
        "version": 1,
        "nodes": [tall("a", 23), tall("b", 3), tall("c", 3), tall("d", 3)],
        "edges": [],
        "meta": {"layout": null}
    });
    apply_layout(&mut doc, LayoutStrategy::Grid).unwrap();
    let pos = |i: usize| {
        let p = &doc["nodes"][i]["position"];
        (p["x"].as_i64().unwrap(), p["y"].as_i64().unwrap())
    };
    // 4 tables → 2 per row: a,b on row 0; c,d on row 1
    assert_eq!(pos(0), (0, 0));
    assert_eq!(pos(1), (320, 0));
    let a_bottom = dibibest_core::erdjson::card_height(23);
    assert!(
        pos(2).1 >= a_bottom + 40,
        "row 1 starts at {}, a ends at {a_bottom}",
        pos(2).1
    );
    assert_eq!(pos(2).0, 0);
    assert_eq!(pos(3), (320, pos(2).1));
}

#[test]
fn grid_keeps_the_old_cell_for_short_tables() {
    let n = |i: usize| {
        serde_json::json!({"id": format!("t{i}"), "name": format!("t{i}"), "position": null,
        "columns": [{"name": "id", "type": "int", "isPk": true, "isFk": false, "isUnique": false, "isNullable": false}]})
    };
    let mut doc = serde_json::json!({"version": 1, "nodes": [n(0), n(1), n(2), n(3)], "edges": [], "meta": {"layout": null}});
    apply_layout(&mut doc, LayoutStrategy::Grid).unwrap();
    assert_eq!(doc["nodes"][2]["position"]["y"], 360);
    assert_eq!(doc["nodes"][3]["position"]["x"], 320);
}
