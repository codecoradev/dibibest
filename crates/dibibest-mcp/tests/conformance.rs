//! MCP protocol conformance: drives the real binary over stdio.

use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};

/// Send newline-delimited requests, return one parsed reply per line.
fn session(reqs: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_dibibest-mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn dibibest-mcp");
    let mut stdin = child.stdin.take().unwrap();
    for r in reqs {
        writeln!(stdin, "{r}").unwrap();
    }
    drop(stdin); // EOF ends the server loop
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).expect("reply is JSON"))
        .collect()
}

fn call(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn one(method: &str, params: Value) -> Value {
    session(&[call(1, method, params)]).remove(0)
}

#[test]
fn initialize_handshake() {
    let r = one("initialize", json!({}));
    assert_eq!(r["jsonrpc"], "2.0");
    assert_eq!(r["id"], 1);
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(r["result"]["serverInfo"]["name"], "dibibest");
    assert_eq!(
        r["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    assert!(r["result"]["capabilities"]["tools"].is_object());
}

#[test]
fn tools_list_shape() {
    let r = one("tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().unwrap();
    let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["convert_schema", "render_diagram", "list_formats"]);
    for t in tools {
        assert!(t["description"].is_string());
        assert_eq!(t["inputSchema"]["type"], "object");
        assert!(t["inputSchema"]["properties"].is_object());
    }
    assert_eq!(
        tools[0]["inputSchema"]["required"],
        json!(["source", "to"]),
        "convert_schema requires source + to"
    );
}

#[test]
fn convert_schema_happy_path() {
    let r = one(
        "tools/call",
        json!({ "name": "convert_schema",
                "arguments": { "source": "Table a { id integer [pk] }", "to": "sql" } }),
    );
    let res = &r["result"];
    assert_eq!(res["isError"], false);
    assert_eq!(res["content"][0]["type"], "text");
    let text = res["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("CREATE TABLE"), "{text}");
    assert!(text.contains("round-trip: lossless"), "{text}");
    assert!(res["structuredContent"]["fidelity"].is_object());
}

#[test]
fn convert_schema_error_is_tool_error_with_code() {
    let r = one(
        "tools/call",
        json!({ "name": "convert_schema",
                "arguments": { "source": "Table a {\n id int", "from": "dbml", "to": "sql" } }),
    );
    assert!(r.get("error").is_none(), "tool failures are not RPC errors");
    assert_eq!(r["result"]["isError"], true);
    let text = r["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("error[unclosed-block]"), "{text}");
}

#[test]
fn unknown_tool_is_tool_error() {
    let r = one("tools/call", json!({ "name": "nope", "arguments": {} }));
    assert_eq!(r["result"]["isError"], true);
}

#[test]
fn unknown_method_is_jsonrpc_error() {
    let r = one("does/not/exist", json!({}));
    assert!(r.get("result").is_none());
    assert_eq!(r["id"], 1);
    assert_eq!(r["error"]["code"], -32601);
    assert!(r["error"]["message"].is_string());
}

#[test]
fn malformed_json_is_parse_error_and_server_survives() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_dibibest-mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{{not json").unwrap();
    writeln!(stdin, "{}", call(7, "tools/list", json!({}))).unwrap();
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let lines: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines[0]["error"]["code"], -32700);
    assert_eq!(lines[0]["id"], Value::Null);
    assert_eq!(lines[1]["id"], 7);
}

#[test]
fn notifications_get_no_reply() {
    let replies = session(&[
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        call(2, "tools/list", json!({})),
    ]);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["id"], 2);
}

#[test]
fn render_diagram_returns_svg() {
    let r = one(
        "tools/call",
        json!({"name": "render_diagram", "arguments": {
            "source": "Table a {\n  id integer [pk]\n}\nTable b {\n  a_id integer\n}\nRef: b.a_id > a.id\n",
            "theme": "light"
        }}),
    );
    let res = &r["result"];
    assert_eq!(res["isError"], false);
    let svg = res["content"][0]["text"].as_str().unwrap();
    assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
    assert!(svg.contains(">a_id<") && svg.contains("marker-end"));
    assert_eq!(res["structuredContent"]["svg"], svg);
    assert!(res["structuredContent"]["width"].as_u64().unwrap() > 0);
}

#[test]
fn render_diagram_errors_are_tool_errors() {
    for args in [
        json!({"source": "hello"}),
        json!({"source": "Table a { id int }", "theme": "pink"}),
    ] {
        let r = one(
            "tools/call",
            json!({"name": "render_diagram", "arguments": args}),
        );
        assert_eq!(r["result"]["isError"], true, "{args}");
    }
}
