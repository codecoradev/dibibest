//! JSON-RPC: the two protocol eras of MCP.
//!
//! - **legacy** (2025-11-25 and earlier): `initialize` negotiates a version; `ping`, `tools/list` and `tools/call`
//!   answer inside a 200, a JSON-RPC error included.
//! - **modern** (2026-07-28): stateless. A request names its version in `_meta`, mirrors it, the method and (for a
//!   tool call) the tool name in headers that have to match the body, and answers carry `resultType` and the
//!   server info. Claude opens with `server/discover` and stays modern.
//!
//! A request is modern when it is not `initialize` and either carries the version in `_meta` or is
//! `server/discover`.

use super::tools;
use axum::http::HeaderMap;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use base64::Engine;
use serde_json::{json, Value};

/// Versions negotiated by `initialize`, newest first.
pub const LEGACY_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];
/// Stateless revisions, served from per-request `_meta`.
pub const MODERN_VERSIONS: [&str; 1] = ["2026-07-28"];
const META_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const TOOLS_TTL_MS: u64 = 300_000;
const DISCOVER_TTL_MS: u64 = 3_600_000;
const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// The per-request protocol version of a modern request, if it names one.
pub fn meta_version(rpc: &Value) -> Option<&str> {
    rpc.get("params")?.get("_meta")?.get(META_KEY)?.as_str()
}

pub fn is_modern(rpc: &Value, method: &str) -> bool {
    method != "initialize" && (meta_version(rpc).is_some() || method == "server/discover")
}

fn supported_versions() -> Vec<&'static str> {
    MODERN_VERSIONS
        .iter()
        .chain(LEGACY_VERSIONS.iter())
        .copied()
        .collect()
}

/// A legacy request. The answer is always a JSON-RPC message sent with status 200.
pub async fn legacy(rpc: &Value, method: &str) -> Value {
    let id = rpc.get("id").cloned().unwrap_or(Value::Null);
    let params = rpc
        .get("params")
        .filter(|p| p.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str);
            let version = asked
                .filter(|v| LEGACY_VERSIONS.contains(v))
                .unwrap_or(LEGACY_VERSIONS[0]);
            result(
                &id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "dibibest", "title": "dibibest — bidirectional schema conversion", "version": VERSION }
                }),
            )
        }
        "ping" => result(&id, json!({})),
        "tools/list" => result(&id, json!({ "tools": tools::definitions() })),
        "tools/call" => result(&id, call_tool(&params).await),
        other => error(&id, -32601, &format!("method not found: {other}")),
    }
}

async fn call_tool(params: &Value) -> Value {
    let name = params.get("name").cloned();
    let args = params
        .get("arguments")
        .filter(|a| a.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    // CPU work: off the async threads, and a panic in it becomes a tool error instead of a dropped connection
    tokio::task::spawn_blocking(move || tools::call(name.as_ref(), &args))
        .await
        .unwrap_or_else(|_| json!({ "content": [{ "type": "text", "text": "error: the tool failed" }], "isError": true }))
}

/// What a modern request answers: the status and the JSON-RPC message.
pub async fn modern(headers: &HeaderMap, rpc: &Value, method: &str) -> (u16, Value) {
    let id = rpc.get("id").cloned().unwrap_or(Value::Null);
    let fail = |status: u16, code: i64, message: &str, data: Option<Value>| {
        let mut e = json!({ "code": code, "message": message });
        if let Some(d) = data {
            e["data"] = d;
        }
        (status, json!({ "jsonrpc": "2.0", "id": id, "error": e }))
    };
    if let Some(version) = meta_version(rpc) {
        if let Some(why) = header_mismatch(headers, rpc, method, version) {
            return fail(400, -32020, &why, None);
        }
        if !MODERN_VERSIONS.contains(&version) {
            return fail(
                400,
                -32022,
                "Unsupported protocol version",
                Some(json!({ "supported": supported_versions(), "requested": version })),
            );
        }
    }
    let params = rpc
        .get("params")
        .filter(|p| p.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut out = match method {
        "server/discover" => json!({
            "supportedVersions": supported_versions(),
            "capabilities": { "tools": {} },
            "ttlMs": DISCOVER_TTL_MS,
            "cacheScope": "public"
        }),
        // private, like uteke-cloud: the answer belongs to an authorised caller
        "tools/list" => {
            json!({ "tools": tools::definitions(), "ttlMs": TOOLS_TTL_MS, "cacheScope": "private" })
        }
        "tools/call" => call_tool(&params).await,
        // ping and the initialize handshake are gone in the modern revision
        other => return fail(404, -32601, &format!("method not found: {other}"), None),
    };
    out["resultType"] = json!("complete");
    out["_meta"] =
        json!({ "io.modelcontextprotocol/serverInfo": { "name": "dibibest", "version": VERSION } });
    (200, result(&id, out))
}

/// `=?base64?...?=` is how a header carries a value that is not plain ASCII; a plain value passes as it is.
fn decode_header(raw: &str) -> Option<String> {
    let Some(inner) = raw
        .strip_prefix("=?base64?")
        .and_then(|r| r.strip_suffix("?="))
    else {
        return Some(raw.to_string());
    };
    let bytes = STANDARD
        .decode(inner)
        .or_else(|_| STANDARD_NO_PAD.decode(inner))
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Streamable HTTP validation of a modern request: the headers that mirror the body must match it (`-32020`).
fn header_mismatch(
    headers: &HeaderMap,
    rpc: &Value,
    method: &str,
    version: &str,
) -> Option<String> {
    let check = |name: &str, expected: &str, decode: bool| -> Option<String> {
        let Some(raw) = headers.get(name).and_then(|v| v.to_str().ok()) else {
            return Some(format!(
                "Header mismatch: required header {name} is missing"
            ));
        };
        let got = if decode {
            decode_header(raw)
        } else {
            Some(raw.to_string())
        };
        match got {
            None => Some(format!(
                "Header mismatch: {name} header has an invalid base64 value"
            )),
            Some(g) if g == expected => None,
            Some(_) => Some(format!(
                "Header mismatch: {name} header does not match the body"
            )),
        }
    };
    check("mcp-protocol-version", version, false)
        .or_else(|| check("mcp-method", method, false))
        .or_else(|| {
            if method != "tools/call" {
                return None;
            }
            let name = rpc
                .get("params")
                .and_then(|p| p.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            check("mcp-name", name, true)
        })
}
