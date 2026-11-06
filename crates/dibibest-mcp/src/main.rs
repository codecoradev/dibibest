//! Minimal MCP (Model Context Protocol) stdio server for dibibest.
//!
//! Implements the JSON-RPC 2.0 surface AI agents need: initialize,
//! tools/list, tools/call. No external MCP SDK dependency — the protocol
//! surface used here is small and stability matters more than features.
//! Roadmap (R5): resources/list for schema previews, official conformance run.

use dibibest_core::diagram::{render_source, RenderOptions, Theme};
use dibibest_core::{detect_format, FormatId, Registry};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn main() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let registry = Registry::with_defaults();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(req) => {
                let Some(id) = req.get("id").cloned() else {
                    continue; // notification (e.g. notifications/initialized): no reply
                };
                let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
                match handle(method, req.get("params"), &registry) {
                    Some(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                    None => rpc_error(id, -32601, format!("method not found: {method}")),
                }
            }
            Err(e) => rpc_error(Value::Null, -32700, format!("parse error: {e}")),
        };
        let mut out = stdout.lock();
        serde_json::to_writer(&mut out, &reply)?;
        out.write_all(b"\n")?;
        out.flush()?;
    }
    Ok(())
}

fn rpc_error(id: Value, code: i32, message: String) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// `None` = method not found (JSON-RPC -32601).
fn handle(method: &str, params: Option<&Value>, registry: &Registry) -> Option<Value> {
    Some(match method {
        "initialize" => json!({
            "protocolVersion": "2025-06-18",
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "dibibest",
                "title": "dibibest — bidirectional schema conversion",
                "version": env!("CARGO_PKG_VERSION"),
            }
        }),
        "tools/list" => json!({
            "tools": [{
                "name": "convert_schema",
                "title": "Convert schema bolak-balik",
                "description": "Convert a database schema between formats (dbml, sql, sql:mysql, sql:sqlite, prisma, erdjson; plain sql = PostgreSQL). \
                                Returns the converted \
                                output plus a fidelity report of what survived.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "source": { "type": "string", "description": "Schema source text (DBML, SQL DDL or Prisma schema)" },
                        "from": { "type": "string", "description": "Source format id; auto-detected when omitted", "enum": ["dbml", "sql", "sql:mysql", "sql:sqlite", "prisma", "erdjson"] },
                        "to": { "type": "string", "description": "Target format id", "enum": ["dbml", "sql", "sql:mysql", "sql:sqlite", "prisma", "erdjson"] }
                    },
                    "required": ["source", "to"]
                }
            },
            {
                "name": "render_diagram",
                "title": "Render the ERD as SVG",
                "description": "Draw the entity-relationship diagram of a schema as a standalone SVG image (tables, columns with PK/FK, relations). Returns the SVG markup; save it as a .svg file or embed it.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "source": { "type": "string", "description": "Schema source text (DBML, SQL DDL, Prisma schema or ERD-JSON)" },
                        "from": { "type": "string", "description": "Source format id; auto-detected when omitted", "enum": ["dbml", "sql", "sql:mysql", "sql:sqlite", "prisma", "erdjson"] },
                        "theme": { "type": "string", "description": "Colour scheme (default dark)", "enum": ["dark", "light"] }
                    },
                    "required": ["source"]
                }
            },
            {
                "name": "list_formats",
                "title": "List supported formats",
                "description": "List format ids the server can convert between, including SQL dialects (sql = PostgreSQL, sql:mysql, sql:sqlite).",
                "inputSchema": { "type": "object", "properties": {} }
            }]
        }),
        "tools/call" => {
            let p = params.cloned().unwrap_or(Value::Null);
            let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = p.get("arguments").cloned().unwrap_or(json!({}));
            tool_call(name, &args, registry)
        }
        _ => return None,
    })
}

fn tool_call(name: &str, args: &Value, registry: &Registry) -> Value {
    match name {
        "list_formats" => {
            json!({ "content": [{ "type": "text", "text": registry.ids().join("\n") }] })
        }
        "render_diagram" => {
            let src = args.get("source").and_then(|s| s.as_str()).unwrap_or("");
            let from = args.get("from").and_then(|s| s.as_str());
            let theme = match args.get("theme").and_then(|s| s.as_str()) {
                None => Some(Theme::Dark),
                Some(t) => Theme::parse(t),
            };
            let Some(theme) = theme else {
                return json!({ "content": [{ "type": "text", "text": "error: `theme` must be dark or light" }], "isError": true });
            };
            let opts = RenderOptions {
                theme,
                ..Default::default()
            };
            match render_source(src, from, &opts) {
                Ok(svg) => json!({
                    "content": [{ "type": "text", "text": svg.svg }],
                    "structuredContent": svg,
                    "isError": false
                }),
                Err(e) => json!({
                    "content": [{ "type": "text", "text": format!("error[{}]: {e}", e.code()) }],
                    "isError": true
                }),
            }
        }
        "convert_schema" => {
            let src = args.get("source").and_then(|s| s.as_str()).unwrap_or("");
            let to = args.get("to").and_then(|s| s.as_str()).unwrap_or("");
            let to_id = FormatId::new(to);
            let result = match args.get("from").and_then(|s| s.as_str()) {
                Some(from) => registry.convert_explicit(src, &FormatId::new(from), &to_id),
                None => match detect_format(src) {
                    Some(from) => registry.convert_explicit(src, &FormatId::new(from), &to_id),
                    None => Err(dibibest_core::CoreError::UnknownFormat(
                        "could not detect source format; pass `from`".into(),
                    )),
                },
            };
            match result {
                Ok(c) => {
                    let mut resp = json!({
                        "content": [{
                            "type": "text",
                            "text": format!("{}\n\n-- fidelity --\ntables {}/{}  columns {}/{}  refs {}/{}  indexes {}/{}  round-trip: {}",
                                c.output,
                                c.fidelity.tables.kept, c.fidelity.tables.total,
                                c.fidelity.columns.kept, c.fidelity.columns.total,
                                c.fidelity.refs.kept, c.fidelity.refs.total,
                                c.fidelity.indexes.kept, c.fidelity.indexes.total,
                                if c.fidelity.idempotent { "lossless" } else { "NOT idempotent" }),
                        }],
                        "isError": false
                    });
                    if let Some(obj) = resp.as_object_mut() {
                        obj.insert(
                            "structuredContent".into(),
                            serde_json::to_value(&c).unwrap_or(Value::Null),
                        );
                    }
                    resp
                }
                Err(e) => json!({
                    "content": [{ "type": "text", "text": format!("error[{}]: {e}", e.code()) }],
                    "isError": true
                }),
            }
        }
        other => {
            json!({ "content": [{ "type": "text", "text": format!("unknown tool: {other}") }], "isError": true })
        }
    }
}
