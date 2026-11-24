//! Replays the MCP contract fixtures (`apps/web/server/fixtures/mcp`, format in its README) against the Rust
//! server. The TypeScript server runs the same files, so both are held to one description of behaviour.
mod common;
use axum::body::Body;
use axum::http::Request;
use common::idp::{Answer, Idp};
use common::keys::Keys;
use common::{app, fixture, send, text};
use dibibest_server::config::Config;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const APP: &str = "https://dibibest.codecora.dev";

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

struct Case {
    id: String,
    doc: Value,
}

fn load(dir: &Path, root: &Path, out: &mut Vec<Case>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            load(&p, root, out);
        } else if p.extension().is_some_and(|e| e == "json") {
            let doc: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
            let id = p
                .strip_prefix(root)
                .unwrap()
                .with_extension("")
                .to_string_lossy()
                .into_owned();
            out.push(Case { id, doc });
        }
    }
}

struct Ctx {
    issuer: String,
}

impl Ctx {
    /// Placeholders: `$issuer`, `$resource`, `$app` inside strings; a whole string `$now`, `$now+N` or `$now-N` is
    /// a number; `{"$repeat": "x", "times": N}` is a string of N copies.
    fn resolve(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => {
                if let Some(rest) = s.strip_prefix("$now") {
                    let offset = if rest.is_empty() {
                        Some(0)
                    } else {
                        rest.parse::<i64>().ok()
                    };
                    if let Some(o) = offset {
                        return json!(now() + o);
                    }
                }
                json!(s
                    .replace("$issuer", &self.issuer)
                    .replace("$resource", &format!("{APP}/mcp"))
                    .replace("$app", APP))
            }
            Value::Array(a) => Value::Array(a.iter().map(|x| self.resolve(x)).collect()),
            Value::Object(o) => {
                if let Some(unit) = o.get("$repeat") {
                    let times = o["times"].as_u64().unwrap() as usize;
                    return json!(unit.as_str().unwrap().repeat(times));
                }
                Value::Object(
                    o.iter()
                        .map(|(k, x)| (k.clone(), self.resolve(x)))
                        .collect(),
                )
            }
            other => other.clone(),
        }
    }
}

fn merge(mut base: Value, over: &Value) -> Value {
    for (k, v) in over.as_object().cloned().unwrap_or_default() {
        base[k] = v;
    }
    base
}

fn mint(ctx: &Ctx, good: &Keys, attacker: &Keys, spec: &Value) -> String {
    let spec = if spec == "valid" {
        json!({})
    } else {
        spec.clone()
    };
    if spec["form"] == "opaque" {
        return "opaque-token-value".into();
    }
    let header = merge(
        json!({"alg": "ES256", "typ": "at+jwt", "kid": "k1"}),
        &ctx.resolve(&spec["header"]),
    );
    let claims = merge(
        json!({"iss": ctx.issuer, "sub": "u1", "aud": [format!("{APP}/mcp"), format!("{}/oauth2/userinfo", ctx.issuer)],
               "exp": now() + 300, "iat": now()}),
        &ctx.resolve(&spec["claims"]),
    );
    let signer = if spec["signedBy"] == "attacker" {
        attacker
    } else {
        good
    };
    if spec["form"] == "tampered" {
        let forged = json!({"iss": ctx.issuer, "sub": "admin", "aud": format!("{APP}/mcp"), "exp": now() + 300});
        return signer.sign_then_swap_payload(&header, &claims, &forged);
    }
    signer.sign(&header, &claims)
}

fn pick<'a>(root: &'a Value, path: &[Value]) -> Option<&'a Value> {
    let mut v = root;
    for k in path {
        v = match k {
            Value::String(s) => v.get(s.as_str())?,
            Value::Number(n) => v.get(n.as_u64()? as usize)?,
            _ => return None,
        };
    }
    Some(v)
}

fn check_body(ctx: &Ctx, body: &Value, checks: &[Value]) -> Result<(), String> {
    for c in checks {
        let path = c["path"].as_array().unwrap();
        let name = path
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(".");
        let mut value = pick(body, path).cloned();
        const OPERATORS: [&str; 7] = [
            "exists",
            "equals",
            "contains",
            "startsWith",
            "containsLine",
            "gt",
            "length",
        ];
        if !OPERATORS.iter().any(|op| c.get(op).is_some()) {
            return Err(format!("body {name}: the check names no operator"));
        }
        if let Some(want) = c.get("exists") {
            if value.is_some() != want.as_bool().unwrap() {
                return Err(format!("body {name}: exists should be {want}"));
            }
            continue;
        }
        let Some(mut v) = value.take() else {
            return Err(format!("body {name}: missing"));
        };
        if let Some(key) = c.get("map").and_then(Value::as_str) {
            v = Value::Array(
                v.as_array()
                    .ok_or(format!("body {name}: not an array"))?
                    .iter()
                    .map(|x| x[key].clone())
                    .collect(),
            );
        }
        if let Some(want) = c.get("equals") {
            let want = ctx.resolve(want);
            if v != want {
                return Err(format!("body {name}: expected {want}, got {v}"));
            }
        }
        if let Some(want) = c.get("contains") {
            let want = ctx.resolve(want);
            let ok = match &v {
                Value::String(s) => s.contains(want.as_str().unwrap_or("\u{0}")),
                Value::Array(a) => a.contains(&want),
                _ => false,
            };
            if !ok {
                return Err(format!(
                    "body {name}: {} does not contain {want}",
                    &v.to_string()[..v.to_string().len().min(200)]
                ));
            }
        }
        if let Some(want) = c.get("startsWith") {
            if !v
                .as_str()
                .is_some_and(|s| s.starts_with(want.as_str().unwrap()))
            {
                return Err(format!("body {name}: does not start with {want}"));
            }
        }
        if let Some(want) = c.get("containsLine") {
            if !v
                .as_str()
                .is_some_and(|s| s.lines().any(|l| l == want.as_str().unwrap()))
            {
                return Err(format!("body {name}: has no line {want}"));
            }
        }
        if let Some(want) = c.get("gt") {
            if !v.as_f64().is_some_and(|n| n > want.as_f64().unwrap()) {
                return Err(format!("body {name}: {v} is not above {want}"));
            }
        }
        if let Some(want) = c.get("length") {
            if v.as_array().map(Vec::len) != want.as_u64().map(|n| n as usize) {
                return Err(format!("body {name}: wrong length"));
            }
        }
    }
    Ok(())
}

async fn run(case: &Case) -> Result<(), String> {
    let idp = Idp::start().await;
    let ctx = Ctx {
        issuer: idp.issuer(),
    };
    let (good, attacker) = (Keys::new(), Keys::new());
    let key_alg = case.doc["config"]["keyAlg"] != false;
    idp.set(
        "jwks",
        Answer::ok(json!({"keys": [good.jwk("k1", key_alg.then_some("ES256"))]})),
    );

    let mut fx = fixture();
    fx.config = Config {
        public_url: Some(APP.into()),
        issuer: (case.doc["config"]["oidc"] != false).then(|| idp.issuer()),
        ..fx.config.clone()
    };
    let router = app(&fx);

    for (n, step) in case.doc["steps"].as_array().unwrap().iter().enumerate() {
        let r = &step["request"];
        let method = r["method"].as_str().unwrap_or("POST");
        let mut req = Request::builder()
            .method(method)
            .uri(r["path"].as_str().unwrap_or("/mcp"));
        req = req
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        for (k, v) in r["headers"].as_object().cloned().unwrap_or_default() {
            req = req.header(k, ctx.resolve(&v).as_str().unwrap());
        }
        let token = step.get("token").cloned().unwrap_or(json!("valid"));
        if !token.is_null() {
            req = req.header(
                "authorization",
                format!("Bearer {}", mint(&ctx, &good, &attacker, &token)),
            );
        }
        let body = match r.get("body") {
            Some(b) if method != "GET" && method != "OPTIONS" => match b.get("$raw") {
                Some(raw) => raw.as_str().unwrap().to_string(),
                None => ctx.resolve(b).to_string(),
            },
            _ => String::new(),
        };
        let res = send(&router, req.body(Body::from(body)).unwrap()).await;

        let want = &step["expect"];
        let status = res.status().as_u16();
        let headers = res.headers().clone();
        let (_, text_body) = text(res).await;
        let at = |what: String| format!("step {}: {what}", n + 1);
        if u64::from(status) != want["status"].as_u64().unwrap() {
            return Err(at(format!(
                "status {status}, expected {}. body: {}",
                want["status"],
                &text_body[..text_body.len().min(300)]
            )));
        }
        for (name, w) in want["headers"].as_object().cloned().unwrap_or_default() {
            let got = headers
                .get(name.as_str())
                .map(|v| v.to_str().unwrap().to_string());
            match (&w, got) {
                (Value::Null, Some(g)) => {
                    return Err(at(format!("header {name} must be absent, got {g}")))
                }
                (Value::Null, None) => {}
                (_, None) => return Err(at(format!("header {name} is missing"))),
                (Value::String(s), Some(g)) if g != ctx.resolve(&json!(s)).as_str().unwrap() => {
                    return Err(at(format!("header {name}: expected {s}, got {g}")));
                }
                (Value::Object(o), Some(g))
                    if !g.contains(ctx.resolve(&o["contains"]).as_str().unwrap()) =>
                {
                    return Err(at(format!("header {name}: {g} lacks {}", o["contains"])));
                }
                (Value::String(_) | Value::Object(_), Some(_)) => {}
                (other, _) => return Err(at(format!("header {name}: unknown check {other}"))),
            }
        }
        if let Some(t) = want.get("bodyText") {
            if text_body != t.as_str().unwrap() {
                return Err(at(format!("body text {text_body:?}")));
            }
        }
        if let Some(checks) = want.get("body") {
            let parsed: Value = serde_json::from_str(&text_body).map_err(|e| {
                at(format!(
                    "body is not JSON ({e}): {}",
                    &text_body[..text_body.len().min(200)]
                ))
            })?;
            check_body(&ctx, &parsed, checks.as_array().unwrap()).map_err(at)?;
        }
    }
    if let Some(n) = case.doc["after"]["jwksFetches"].as_u64() {
        let got = idp.calls_to("/jwks").len() as u64;
        if got != n {
            return Err(format!("the keys were fetched {got} times, expected {n}"));
        }
    }
    Ok(())
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/web/server/fixtures/mcp")
}

#[tokio::test]
async fn every_fixture_passes() {
    let root = fixtures_dir();
    let mut cases = Vec::new();
    load(&root, &root, &mut cases);
    let groups: std::collections::BTreeSet<&str> = cases
        .iter()
        .filter_map(|c| c.id.split('/').next())
        .collect();
    for g in [
        "metadata",
        "auth",
        "transport",
        "cors",
        "legacy",
        "modern",
        "tools",
    ] {
        assert!(groups.contains(g), "no fixtures found in the group {g}");
    }
    assert!(cases.len() >= 55, "only {} fixtures found", cases.len());
    let mut failures = Vec::new();
    for case in &cases {
        if let Err(why) = run(case).await {
            failures.push(format!("{}: {why}", case.id));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} fixtures failed:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
