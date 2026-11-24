//! The three MCP tools, as thin wrappers over `dibibest-core`. The text of every answer is the one the TypeScript
//! server gave (and the stdio server in `dibibest-mcp` is close to), so a client sees no difference.

use dibibest_core::diagram::{render_source, RenderOptions, Theme};
use dibibest_core::registry::Fidelity;
use dibibest_core::{default_registry, detect_format, FormatId};
use serde_json::{json, Value};

const FORMATS: [&str; 6] = [
    "dbml",
    "sql",
    "sql:mysql",
    "sql:sqlite",
    "prisma",
    "erdjson",
];

fn annotations() -> Value {
    json!({ "readOnlyHint": true, "openWorldHint": false })
}

pub fn definitions() -> Value {
    json!([
        {
            "name": "convert_schema",
            "title": "Convert schema (round-trip)",
            "description": "Convert a database schema between formats (dbml, sql, sql:mysql, sql:sqlite, prisma, erdjson; plain sql = PostgreSQL). Returns the converted output plus a fidelity report of what survived.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": { "type": "string", "description": "Schema source text (DBML, SQL DDL or Prisma schema)" },
                    "from": { "type": "string", "description": "Source format id; auto-detected when omitted", "enum": FORMATS },
                    "to": { "type": "string", "description": "Target format id", "enum": FORMATS }
                },
                "required": ["source", "to"]
            },
            "annotations": annotations()
        },
        {
            "name": "render_diagram",
            "title": "Render the ERD as SVG",
            "description": "Draw the entity-relationship diagram of a schema as a standalone SVG image (tables, columns with PK/FK, relations). Returns the SVG markup; save it as a .svg file or embed it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": { "type": "string", "description": "Schema source text (DBML, SQL DDL, Prisma schema or ERD-JSON)" },
                    "from": { "type": "string", "description": "Source format id; auto-detected when omitted", "enum": FORMATS },
                    "theme": { "type": "string", "description": "Colour scheme (default dark)", "enum": ["dark", "light"] }
                },
                "required": ["source"]
            },
            "annotations": annotations()
        },
        {
            "name": "list_formats",
            "title": "List supported formats",
            "description": "List format ids the server can convert between, including SQL dialects (sql = PostgreSQL, sql:mysql, sql:sqlite).",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": annotations()
        }
    ])
}

fn tool_error(text: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }], "isError": true })
}

/// What a tool call answers. It does CPU work, so the caller runs it off the async threads.
pub fn call(name: Option<&Value>, args: &Value) -> Value {
    match name.and_then(Value::as_str) {
        Some("list_formats") => json!({
            "content": [{ "type": "text", "text": default_registry().ids().join("\n") }],
            "isError": false
        }),
        Some("render_diagram") => render_diagram(args),
        Some("convert_schema") => convert_schema(args),
        Some(other) => tool_error(format!("unknown tool: {other}")),
        None => tool_error(format!(
            "unknown tool: {}",
            name.map_or_else(|| "undefined".to_string(), Value::to_string)
        )),
    }
}

/// The optional string argument `key`: `Ok(None)` if absent, an error if present and not a string.
fn optional_text<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, ()> {
    match args.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(()),
    }
}

fn render_diagram(args: &Value) -> Value {
    let bad = || {
        tool_error("error: `source` must be a string, `from` a format id and `theme` dark or light")
    };
    let Some(source) = args.get("source").and_then(Value::as_str) else {
        return bad();
    };
    let Ok(from) = optional_text(args, "from") else {
        return bad();
    };
    let theme = match args.get("theme") {
        None => Theme::Dark,
        Some(Value::String(t)) if t == "dark" || t == "light" => {
            Theme::parse(t).unwrap_or(Theme::Dark)
        }
        Some(_) => return bad(),
    };
    let from = from.filter(|f| !f.is_empty() && *f != "auto");
    let opts = RenderOptions {
        theme,
        ..Default::default()
    };
    match render_source(source, from, &opts) {
        Ok(svg) => json!({
            "content": [{ "type": "text", "text": svg.svg }],
            "structuredContent": { "svg": svg.svg, "width": svg.width, "height": svg.height },
            "isError": false
        }),
        Err(e) => tool_error(format!("error: {e}")),
    }
}

fn convert_schema(args: &Value) -> Value {
    let bad = || tool_error("error: `source` and `to` must be strings (and `from`, when given)");
    let (Some(source), Some(to)) = (
        args.get("source").and_then(Value::as_str),
        args.get("to").and_then(Value::as_str),
    ) else {
        return bad();
    };
    let Ok(from) = optional_text(args, "from") else {
        return bad();
    };
    let from = match from.filter(|f| *f != "auto") {
        Some(f) => f.to_string(),
        None => {
            match detect_format(source) {
                Some(f) => f.to_string(),
                None => {
                    return tool_error("error: could not detect the format; expected DBML, SQL, Prisma or ERD-JSON");
                }
            }
        }
    };
    match default_registry().convert_explicit(source, &FormatId::new(from), &FormatId::new(to)) {
        Ok(c) => {
            let text = format!(
                "{}\n\n-- fidelity --\n{}",
                c.output,
                fidelity_text(&c.fidelity)
            );
            json!({
                "content": [{ "type": "text", "text": text }],
                "structuredContent": { "output": c.output, "fidelity": c.fidelity },
                "isError": false
            })
        }
        Err(e) => tool_error(format!("error: {e}")),
    }
}

/// The same line as the stdio server and the TypeScript server wrote, so every transport reads alike.
fn fidelity_text(f: &Fidelity) -> String {
    format!(
        "tables {}/{}  columns {}/{}  refs {}/{}  indexes {}/{}  round-trip: {}",
        f.tables.kept,
        f.tables.total,
        f.columns.kept,
        f.columns.total,
        f.refs.kept,
        f.refs.total,
        f.indexes.kept,
        f.indexes.total,
        if f.idempotent {
            "lossless"
        } else {
            "NOT idempotent"
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use dibibest_core::registry::Outcome;

    fn fidelity(idempotent: bool) -> Fidelity {
        Fidelity {
            tables: Outcome { kept: 1, total: 2 },
            columns: Outcome { kept: 3, total: 4 },
            refs: Outcome { kept: 5, total: 6 },
            indexes: Outcome { kept: 7, total: 8 },
            idempotent,
            ..Default::default()
        }
    }

    #[test]
    fn the_fidelity_line_names_every_count_in_order() {
        assert_eq!(
            fidelity_text(&fidelity(true)),
            "tables 1/2  columns 3/4  refs 5/6  indexes 7/8  round-trip: lossless"
        );
    }

    #[test]
    fn a_round_trip_that_is_not_idempotent_says_so() {
        assert!(fidelity_text(&fidelity(false)).ends_with("round-trip: NOT idempotent"));
    }
}
