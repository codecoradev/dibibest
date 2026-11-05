//! DBML conformance corpus: every `tests/corpus/*.dbml` must survive
//! parse → emit → re-parse with an identical IR and a stable emission.
//! The suite fails below 95% (issue #6 / ROADMAP M1).

use dibibest_core::{FormatId, Registry};
use std::fs;
use std::path::Path;

const MIN_PASS_RATE: f64 = 95.0;

fn check(reg: &Registry, src: &str) -> Result<(), String> {
    let dbml = reg
        .codec(&FormatId::new("dbml"))
        .map_err(|e| e.to_string())?;
    let ir1 = dbml.parse(src).map_err(|e| format!("parse: {e}"))?;
    let out1 = dbml.emit(&ir1).map_err(|e| format!("emit: {e}"))?;
    let ir2 = dbml
        .parse(&out1)
        .map_err(|e| format!("re-parse of emitted output: {e}"))?;
    if ir1 != ir2 {
        return Err("IR changed across round-trip".into());
    }
    let out2 = dbml.emit(&ir2).map_err(|e| format!("emit: {e}"))?;
    if out1 != out2 {
        return Err("emit is not stable (emit(ir2) != emit(ir1))".into());
    }
    Ok(())
}

#[test]
fn corpus_round_trips_at_95_percent() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut files: Vec<_> = fs::read_dir(&dir)
        .expect("corpus dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "dbml"))
        .collect();
    files.sort();
    assert!(files.len() >= 12, "corpus must hold at least 12 files");

    let reg = Registry::with_defaults();
    let mut passed = 0;
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy();
        let src = fs::read_to_string(f).expect("read corpus file");
        match check(&reg, &src) {
            Ok(()) => {
                passed += 1;
                println!("PASS  {name}");
            }
            Err(e) => println!("FAIL  {name}  — {e}"),
        }
    }
    let rate = passed as f64 * 100.0 / files.len() as f64;
    println!(
        "corpus pass-rate: {}/{} ({:.1}%)",
        passed,
        files.len(),
        rate
    );
    assert!(
        rate >= MIN_PASS_RATE,
        "corpus pass-rate {rate:.1}% is below {MIN_PASS_RATE}%"
    );
}

#[test]
fn unknown_settings_are_preserved_and_round_trip() {
    let reg = Registry::with_defaults();
    let dbml = reg.codec(&FormatId::new("dbml")).unwrap();
    let src = "Table t [headercolor: #fff] {\n  id int [pk, color: red]\n  \
               indexes {\n    (id) [type: hash]\n  }\n}\nRef: t.id > t.id [delete: cascade, color: #f00]\n";
    let ir = dbml.parse(src).unwrap();
    assert_eq!(ir.tables[0].columns[0].unknown, ["color: red"]);
    assert_eq!(
        ir.tables[0].unknown,
        ["headercolor: #fff", "type: hash"],
        "table + index unknowns"
    );
    // `delete:` is a referential action on the ref (#43); only the truly
    // unknown ref setting stays in the passthrough store.
    assert_eq!(ir.refs[0].on_delete.as_deref(), Some("cascade"));
    assert_eq!(ir.unknown, ["color: #f00"]);
    let out = dbml.emit(&ir).unwrap();
    assert!(out.contains("// unknown: color: red"));
    assert!(out.contains("Ref: t.id > t.id [delete: cascade]"));
    assert!(out.contains("// unknown: color: #f00"));
    assert_eq!(dbml.parse(&out).unwrap(), ir);
}
