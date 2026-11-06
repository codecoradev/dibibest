//! WASM bridge for the web canvas. No wasm-bindgen: a three-function C ABI
//! (`alloc`, `dealloc`, `run`) keeps the build to plain `cargo` and the JS side
//! to ~30 lines. All logic lives in [`handle`], which is plain Rust and tested natively.
//!
//! Request (JSON): `{"op":"erd"|"convert","src":..,"from":..,"to":..}`; `from` may
//! be `"auto"` to use the core's format detection (file import).
//! `{"op":"formats"}` lists the registry's format ids as `{"ok":true,"formats":[..]}`.
//! `{"op":"svg","erd":<ERD-JSON>|"src":..,"from":..,"theme":"dark"|"light","hidden":[ids]}`
//! draws the diagram: `{"ok":true,"svg":..,"width":..,"height":..}`.
//! Response (JSON): `{"ok":true,"output":..,"fidelity":{..},"from":..}` or
//! `{"ok":false,"error":..}`. `from` echoes the format actually used.

use dibibest_core::diagram::{render_source, render_svg, RenderOptions, Theme};
use dibibest_core::erdjson::{apply_layout, LayoutStrategy};
use dibibest_core::{default_registry, detect_format, FormatId};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct Req {
    op: String,
    #[serde(default)]
    src: String,
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: Option<String>,
    /// `svg`: an ERD-JSON document with canvas positions (instead of `src`).
    #[serde(default)]
    erd: Option<Value>,
    #[serde(default)]
    theme: Option<String>,
    #[serde(default)]
    hidden: Vec<String>,
}

/// `erd`: source → ERD-JSON with grid layout applied (`output` is the document).
/// `convert`: source → `to` text. Both carry the real core `Fidelity`.
pub fn handle(req: &str) -> String {
    let out = serde_json::from_str::<Req>(req)
        .map_err(|e| format!("bad request: {e}"))
        .and_then(run_req);
    match out {
        Ok(v) => v,
        Err(e) => json!({"ok": false, "error": e}),
    }
    .to_string()
}

fn run_req(r: Req) -> Result<Value, String> {
    let reg = default_registry();
    if r.op == "formats" {
        return Ok(json!({"ok": true, "formats": reg.ids()}));
    }
    if r.op == "svg" {
        let theme = match r.theme.as_deref() {
            None => Theme::Dark,
            Some(t) => Theme::parse(t).ok_or(format!("unknown theme: {t}"))?,
        };
        let opts = RenderOptions {
            theme,
            hidden: r.hidden.into_iter().collect(),
        };
        let svg = match &r.erd {
            Some(doc) => render_svg(doc, &opts).ok_or("no tables to draw")?,
            None => {
                let from = (!r.from.is_empty() && r.from != "auto").then_some(r.from.as_str());
                render_source(&r.src, from, &opts).map_err(|e| e.to_string())?
            }
        };
        return Ok(json!({"ok": true, "svg": svg.svg, "width": svg.width, "height": svg.height}));
    }
    let from_id = if r.from == "auto" {
        detect_format(&r.src)
            .ok_or("could not detect the format; expected DBML, SQL, Prisma or ERD-JSON")?
            .to_string()
    } else {
        r.from
    };
    let from = FormatId::new(from_id.clone());
    let to = match r.op.as_str() {
        "erd" => FormatId::new("erdjson"),
        "convert" => FormatId::new(r.to.ok_or("convert needs `to`")?),
        op => return Err(format!("unknown op: {op}")),
    };
    let conv = reg
        .convert_explicit(&r.src, &from, &to)
        .map_err(|e| e.to_string())?;
    let output = if r.op == "erd" {
        let mut doc: Value = serde_json::from_str(&conv.output).map_err(|e| e.to_string())?;
        apply_layout(&mut doc, LayoutStrategy::Grid).map_err(|e| e.to_string())?;
        doc.to_string()
    } else {
        conv.output
    };
    Ok(json!({"ok": true, "output": output, "fidelity": conv.fidelity, "from": from_id}))
}

#[no_mangle]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    leak(vec![0; len])
}

/// Boxed slices have capacity == len by construction, so `dealloc` can rebuild them exactly.
fn leak(v: Vec<u8>) -> *mut u8 {
    Box::into_raw(v.into_boxed_slice()) as *mut u8
}

/// # Safety
/// `ptr`/`len` must come from [`alloc`] (or a [`run`] result) with the same `len`.
#[no_mangle]
pub unsafe extern "C" fn dealloc(ptr: *mut u8, len: usize) {
    drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)));
}

/// Returns a buffer `[len: u32 LE][json bytes]`; free it with `dealloc(ptr, 4 + len)`.
///
/// # Safety
/// `ptr`/`len` must describe a live buffer of UTF-8 bytes written by the caller.
#[no_mangle]
pub unsafe extern "C" fn run(ptr: *const u8, len: usize) -> *mut u8 {
    let req = String::from_utf8_lossy(std::slice::from_raw_parts(ptr, len));
    let body = handle(&req).into_bytes();
    let mut buf = Vec::with_capacity(4 + body.len());
    buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
    buf.extend_from_slice(&body);
    leak(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(v: Value) -> Value {
        serde_json::from_str(&handle(&v.to_string())).unwrap()
    }

    #[test]
    fn erd_carries_layout_and_fidelity() {
        let r = call(json!({"op":"erd","from":"dbml","src":"Table a { id integer [pk] }"}));
        assert_eq!(r["ok"], true);
        let doc: Value = serde_json::from_str(r["output"].as_str().unwrap()).unwrap();
        assert_eq!(doc["meta"]["layout"], "grid");
        assert_eq!(r["fidelity"]["tables"]["kept"], 1);
        assert_eq!(r["fidelity"]["idempotent"], true);
    }

    #[test]
    fn convert_and_errors() {
        let r =
            call(json!({"op":"convert","from":"dbml","to":"sql","src":"Table a { id integer }"}));
        assert!(r["output"].as_str().unwrap().contains("CREATE TABLE"));
        assert_eq!(
            call(json!({"op":"convert","from":"dbml","src":""}))["ok"],
            false
        );
        assert_eq!(call(json!({"op":"x","from":"dbml","src":""}))["ok"], false);
        assert!(handle("nope").contains("bad request"));
    }

    #[test]
    fn lists_formats() {
        let r = call(json!({"op":"formats"}));
        assert_eq!(r["ok"], true);
        let ids: Vec<&str> = r["formats"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(ids.contains(&"dbml") && ids.contains(&"sql:mysql"));
    }

    #[test]
    fn draws_svg_from_a_document_or_from_source() {
        let doc = json!({"version":1,"nodes":[{"id":"a","name":"a","position":{"x":0,"y":0},
            "columns":[{"name":"id","type":"int","isPk":true}]}],"edges":[],"meta":{"layout":null}});
        let r = call(json!({"op":"svg","erd":doc,"theme":"light"}));
        assert_eq!(r["ok"], true);
        assert!(r["svg"].as_str().unwrap().contains(">a<"));
        assert_eq!(r["width"], 300);
        let r = call(json!({"op":"svg","src":"Table t {\n  id int [pk]\n}\n","from":"auto"}));
        assert!(r["svg"].as_str().unwrap().contains(">t<"));
        assert_eq!(
            call(json!({"op":"svg","erd":doc,"theme":"pink"}))["ok"],
            false
        );
        assert_eq!(
            call(json!({"op":"svg","erd":doc,"hidden":["a"]}))["ok"],
            false
        );
    }

    #[test]
    fn auto_detects_the_source_format() {
        let r =
            call(json!({"op":"erd","from":"auto","src":"CREATE TABLE a (id int PRIMARY KEY);"}));
        assert_eq!(r["ok"], true);
        assert_eq!(r["from"], "sql");
        let r = call(json!({"op":"erd","from":"auto","src":"Table a {\n  id int [pk]\n}\n"}));
        assert_eq!(r["from"], "dbml");
        assert_eq!(
            call(json!({"op":"erd","from":"auto","src":"hello"}))["ok"],
            false
        );
    }
}
