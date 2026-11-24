//! What the shared MCP fixtures do not pin: the public address as the source of the resource URL, the body limit
//! to the byte, the shapes of an `Authorization` header, and the provider-metadata cache.
mod common;
use axum::body::Body;
use axum::http::{Request, Response};
use axum::Router;
use common::idp::{Answer, Idp};
use common::keys::Keys;
use common::{app, fixture, send, text};
use dibibest_server::auth::http::client;
use dibibest_server::auth::provider::Provider;
use dibibest_server::config::Config;
use serde_json::{json, Value};
use std::time::Duration;

const APP: &str = "https://dibibest.codecora.dev";

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

struct World {
    router: Router,
    idp: Idp,
    keys: Keys,
    _fx: common::Fixture,
}

async fn world() -> World {
    let idp = Idp::start().await;
    let keys = Keys::new();
    idp.set(
        "jwks",
        Answer::ok(json!({"keys": [keys.jwk("k1", Some("ES256"))]})),
    );
    let mut fx = fixture();
    fx.config = Config {
        public_url: Some(APP.into()),
        issuer: Some(idp.issuer()),
        ..fx.config.clone()
    };
    World {
        router: app(&fx),
        idp,
        keys,
        _fx: fx,
    }
}

impl World {
    fn token(&self) -> String {
        self.keys.sign(
            &json!({"alg": "ES256", "typ": "at+jwt", "kid": "k1"}),
            &json!({"iss": self.idp.issuer(), "sub": "u1", "aud": format!("{APP}/mcp"), "exp": now() + 300, "iat": now()}),
        )
    }

    async fn post(&self, body: String, headers: &[(&str, String)]) -> Response<Body> {
        let mut req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json");
        for (k, v) in headers {
            req = req.header(*k, v);
        }
        send(&self.router, req.body(Body::from(body)).unwrap()).await
    }

    async fn rpc(&self, message: Value) -> (u16, Value) {
        let res = self
            .post(
                message.to_string(),
                &[("authorization", format!("Bearer {}", self.token()))],
            )
            .await;
        let (status, body) = text(res).await;
        (
            status.as_u16(),
            serde_json::from_str(&body).unwrap_or(Value::Null),
        )
    }
}

fn header(res: &Response<Body>, name: &str) -> String {
    res.headers()
        .get(name)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default()
}

#[tokio::test]
async fn the_resource_and_the_challenge_come_from_the_public_address_not_from_the_host_header() {
    let w = world().await;
    let get = Request::builder()
        .uri("/.well-known/oauth-protected-resource/mcp")
        .header("host", "evil.example")
        .header("x-forwarded-host", "evil.example")
        .body(Body::empty())
        .unwrap();
    let (_, body) = text(send(&w.router, get).await).await;
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["resource"],
        format!("{APP}/mcp")
    );

    let res = w
        .post("{}".into(), &[("host", "evil.example".into())])
        .await;
    assert_eq!(res.status(), 401);
    let challenge = header(&res, "www-authenticate");
    assert!(
        challenge.contains(&format!("{APP}/.well-known/oauth-protected-resource/mcp")),
        "{challenge}"
    );
    assert!(!challenge.contains("evil.example"), "{challenge}");
}

#[tokio::test]
async fn a_token_for_a_resource_on_another_host_is_refused() {
    let w = world().await;
    let token = w.keys.sign(
        &json!({"alg": "ES256", "kid": "k1"}),
        &json!({"iss": w.idp.issuer(), "sub": "u1", "aud": "https://evil.example/mcp", "exp": now() + 300}),
    );
    let res = w
        .post(
            "{}".into(),
            &[
                ("authorization", format!("Bearer {token}")),
                ("host", "evil.example".into()),
            ],
        )
        .await;
    assert_eq!(res.status(), 401);
    assert!(
        header(&res, "www-authenticate").contains("error=\"invalid_token\""),
        "refused for its audience, not for something else"
    );

    // control: the same request with a token for this server's own address is let through, whatever Host says
    let ok = w
        .post(
            "{}".into(),
            &[
                ("authorization", format!("Bearer {}", w.token())),
                ("host", "evil.example".into()),
            ],
        )
        .await;
    assert_ne!(ok.status(), 401);
}

#[tokio::test]
async fn without_a_public_address_the_endpoint_is_not_configured() {
    let idp = Idp::start().await;
    let mut fx = fixture();
    fx.config = Config {
        issuer: Some(idp.issuer()),
        public_url: None,
        ..fx.config.clone()
    };
    let router = app(&fx);
    for (method, path) in [
        ("POST", "/mcp"),
        ("GET", "/.well-known/oauth-protected-resource"),
        ("GET", "/.well-known/openid-configuration"),
    ] {
        let res = send(
            &router,
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(res.status(), 404, "{method} {path}");
    }
}

#[tokio::test]
async fn the_body_limit_is_512_kib_to_the_byte() {
    let w = world().await;
    let auth = [("authorization", format!("Bearer {}", w.token()))];
    let notification = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    let at_limit = format!(
        "{notification}{}",
        " ".repeat(512 * 1024 - notification.len())
    );
    assert_eq!(at_limit.len(), 512 * 1024);
    assert_eq!(
        w.post(at_limit, &auth).await.status(),
        202,
        "exactly at the limit is accepted"
    );
    let over = format!(
        "{notification}{}",
        " ".repeat(512 * 1024 + 1 - notification.len())
    );
    assert_eq!(
        w.post(over, &auth).await.status(),
        413,
        "one byte over is refused"
    );
}

#[tokio::test]
async fn which_authorization_headers_count_as_a_bearer_token() {
    let w = world().await;
    let t = w.token();
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();
    for (value, ok) in [
        (format!("Bearer {t}"), true),
        (format!("bearer {t}"), true),
        (format!("BEARER   {t}  "), true),
        (format!("Basic {t}"), false),
        (format!("Bearer {t} extra"), false),
        (format!("Bearer {t}!"), false),
        ("Bearer".to_string(), false),
        ("Bearer ".to_string(), false),
        (t.clone(), false),
    ] {
        let res = w
            .post(ping.clone(), &[("authorization", value.clone())])
            .await;
        assert_eq!(res.status() == 200, ok, "{value:?} gave {}", res.status());
        if !ok {
            assert_eq!(res.status(), 401, "{value:?}");
            // it was refused as "not a bearer token", not as a bad token: the challenge carries no error
            assert!(
                !header(&res, "www-authenticate").contains("error="),
                "{value:?}: {}",
                header(&res, "www-authenticate")
            );
        }
    }
    assert_eq!(w.post(ping, &[]).await.status(), 401, "no header at all");
}

#[tokio::test]
async fn the_shape_of_a_json_rpc_message_decides_how_it_is_answered() {
    let w = world().await;
    // a null id is still an id: it is a request, and it is answered
    let (status, body) = w
        .rpc(json!({"jsonrpc": "2.0", "id": null, "method": "ping"}))
        .await;
    assert_eq!((status, &body["result"]), (200, &json!({})));
    // no method: a response from the client, no body
    let res = w
        .post(
            json!({"jsonrpc": "2.0", "id": 1, "result": {}}).to_string(),
            &[("authorization", format!("Bearer {}", w.token()))],
        )
        .await;
    assert_eq!(res.status(), 202);
    // a method that is not text
    let (status, body) = w.rpc(json!({"jsonrpc": "2.0", "id": 7, "method": 5})).await;
    assert_eq!(
        (status, &body["error"]["code"], &body["id"]),
        (400, &json!(-32600), &json!(7))
    );
    // not JSON-RPC 2.0
    for bad in [
        json!({"id": 1, "method": "ping"}),
        json!({"jsonrpc": "1.0", "id": 1, "method": "ping"}),
        json!(5),
        json!("x"),
        json!(null),
    ] {
        let (status, body) = w.rpc(bad.clone()).await;
        assert_eq!(
            (status, &body["error"]["code"]),
            (400, &json!(-32600)),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn delete_and_other_methods_are_not_allowed_and_say_what_is() {
    let w = world().await;
    for method in ["GET", "DELETE", "PUT", "PATCH"] {
        let res = send(
            &w.router,
            Request::builder()
                .method(method)
                .uri("/mcp")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(res.status(), 405, "{method}");
        assert_eq!(header(&res, "allow"), "POST, OPTIONS");
    }
}

#[tokio::test]
async fn an_origin_is_echoed_with_vary_and_never_with_credentials() {
    let w = world().await;
    let res = w
        .post("{}".into(), &[("origin", "https://claude.ai".into())])
        .await;
    assert_eq!(
        header(&res, "access-control-allow-origin"),
        "https://claude.ai"
    );
    assert_eq!(header(&res, "vary"), "origin");
    assert!(
        res.headers()
            .get("access-control-allow-credentials")
            .is_none(),
        "no cookie is ever read, so none is allowed"
    );
}

#[tokio::test]
async fn the_unreachable_provider_is_a_503_with_a_retry_hint_not_a_401() {
    let w = world().await;
    w.idp.set(
        "discovery",
        Answer {
            status: 500,
            ..Answer::ok(json!({}))
        },
    );
    let res = w
        .post(
            "{}".into(),
            &[("authorization", format!("Bearer {}", w.token()))],
        )
        .await;
    assert_eq!(res.status(), 503);
    assert_eq!(header(&res, "retry-after"), "30");
}

#[tokio::test]
async fn the_providers_metadata_is_passed_on_unchanged_and_kept() {
    let w = world().await;
    let mut doc = w.idp.state.discovery.lock().unwrap().json.clone().unwrap();
    doc["x_extra_field"] = json!({"kept": true});
    w.idp.set("discovery", Answer::ok(doc));
    for _ in 0..2 {
        let res = send(
            &w.router,
            Request::builder()
                .uri("/.well-known/oauth-authorization-server/mcp")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(header(&res, "access-control-allow-origin"), "*");
        assert_eq!(header(&res, "cache-control"), "public, max-age=300");
        let (_, body) = text(res).await;
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["x_extra_field"],
            json!({"kept": true})
        );
    }
    assert_eq!(
        w.idp.calls_to("/.well-known/openid-configuration").len(),
        1,
        "the second answer came from memory"
    );
    assert_eq!(
        w.idp
            .calls_to("/.well-known/oauth-authorization-server")
            .len(),
        1,
        "the RFC 8414 address is tried first"
    );
}

#[tokio::test]
async fn a_provider_with_no_usable_metadata_is_a_502() {
    let idp = Idp::start().await;
    idp.set(
        "discovery",
        Answer::ok(json!({
            "issuer": 5,
            "authorization_endpoint": format!("{}/oauth2/authorize", idp.issuer())
        })),
    );
    let p = Provider::new(&idp.issuer(), client(Duration::from_secs(5)));
    assert!(
        p.authorization_server_metadata().await.is_none(),
        "a document without a text issuer is not accepted"
    );
    let mut fx = fixture();
    fx.config = Config {
        public_url: Some(APP.into()),
        issuer: Some(idp.issuer()),
        ..fx.config.clone()
    };
    let res = send(
        &app(&fx),
        Request::builder()
            .uri("/.well-known/openid-configuration")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.status(), 502);
}

#[tokio::test]
async fn keys_that_cannot_be_fetched_make_a_503_and_not_a_401() {
    let w = world().await;
    w.idp.set(
        "jwks",
        Answer {
            status: 500,
            ..Answer::ok(json!({}))
        },
    );
    let res = w
        .post(
            "{}".into(),
            &[("authorization", format!("Bearer {}", w.token()))],
        )
        .await;
    assert_eq!(res.status(), 503);
    assert_eq!(header(&res, "retry-after"), "30");
}

#[tokio::test]
async fn initialize_stays_in_the_legacy_era_even_when_it_names_a_modern_version() {
    let w = world().await;
    let (status, body) = w
        .rpc(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}}}))
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["result"]["protocolVersion"], "2025-06-18");
    assert!(body["result"].get("resultType").is_none(), "{body}");
}

#[tokio::test]
async fn an_unknown_theme_is_refused_for_a_schema_that_is_otherwise_fine() {
    let w = world().await;
    let call = |theme: &str| {
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "render_diagram", "arguments": {"source": "Table a {\n  id integer\n}\n", "theme": theme}}})
    };
    assert_eq!(w.rpc(call("light")).await.1["result"]["isError"], false);
    assert_eq!(w.rpc(call("dark")).await.1["result"]["isError"], false);
    let (_, pink) = w.rpc(call("pink")).await;
    assert_eq!(pink["result"]["isError"], true);
    assert!(
        pink["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("dark or light"),
        "{pink}"
    );
}

// ---- one log line per call ----

#[derive(Clone, Default)]
struct LogBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuf {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Collects what the server logs while the returned guard lives (these tests run on one thread).
fn capture_log() -> (LogBuf, tracing::subscriber::DefaultGuard) {
    let buf = LogBuf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .finish();
    (buf, tracing::subscriber::set_default(subscriber))
}

fn lines_about_mcp(buf: &LogBuf) -> Vec<String> {
    String::from_utf8(buf.0.lock().unwrap().clone())
        .unwrap()
        .lines()
        .filter(|l| l.contains("mcp:"))
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn every_call_leaves_one_line_with_status_method_agent_version_and_reason() {
    let w = world().await;
    let (buf, _guard) = capture_log();
    let good = format!("Bearer {}", w.token());
    let ua = ("user-agent", "claude-test/1.0".to_string());
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string();

    struct Case(
        &'static str,
        String,
        Vec<(&'static str, String)>,
        Vec<&'static str>,
    );
    // (name, body, headers, the words that must be in its one line)
    let cases = vec![
        Case(
            "no token",
            list.clone(),
            vec![ua.clone()],
            vec!["status=401", "no bearer token", "claude-test/1.0"],
        ),
        Case(
            "bad token",
            list.clone(),
            vec![
                ua.clone(),
                ("authorization", "Bearer abc.def.ghi".to_string()),
            ],
            vec!["status=401", "token refused"],
        ),
        Case(
            "tools/list",
            list.clone(),
            vec![
                ua.clone(),
                ("authorization", good.clone()),
                ("mcp-protocol-version", "2025-06-18".to_string()),
            ],
            vec![
                "status=200",
                "method=\"tools/list\"",
                "pv=\"2025-06-18\"",
                "claude-test/1.0",
                "reason=-",
            ],
        ),
        Case(
            "parse error",
            "{not json".to_string(),
            vec![ua.clone(), ("authorization", good.clone())],
            vec!["status=400", "parse error"],
        ),
        Case(
            "too large",
            "x".repeat(512 * 1024 + 1),
            vec![ua.clone(), ("authorization", good.clone())],
            vec!["status=413", "request too large"],
        ),
        Case(
            "far too large",
            "x".repeat(512 * 1024 + 100),
            vec![ua.clone(), ("authorization", good.clone())],
            vec!["status=413", "request too large"],
        ),
        Case(
            "too large by its declared length",
            "{}".to_string(),
            vec![
                ua.clone(),
                ("authorization", good.clone()),
                ("content-length", (512 * 1024 + 1).to_string()),
            ],
            vec!["status=413", "request too large"],
        ),
        Case(
            "not allowed",
            String::new(),
            vec![ua.clone()],
            vec!["status=405", "method not allowed"],
        ),
    ];
    for Case(name, body, headers, wanted) in cases {
        let before = lines_about_mcp(&buf).len();
        if name == "not allowed" {
            let req = Request::builder()
                .method("DELETE")
                .uri("/mcp")
                .header("user-agent", "claude-test/1.0")
                .body(Body::empty())
                .unwrap();
            send(&w.router, req).await;
        } else {
            w.post(body, &headers).await;
        }
        let lines = lines_about_mcp(&buf);
        assert_eq!(lines.len(), before + 1, "{name}: one line per call");
        let line = lines.last().unwrap();
        for word in wanted {
            assert!(line.contains(word), "{name}: {word} missing in {line}");
        }
    }
}

#[tokio::test]
async fn the_log_never_holds_a_token_or_a_request_body_and_a_method_name_cannot_split_a_line() {
    let w = world().await;
    let (buf, _guard) = capture_log();
    let token = w.token();
    w.post(
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": "convert_schema", "arguments": {"input": "SECRET-SCHEMA-TEXT"}}})
        .to_string(),
        &[("authorization", format!("Bearer {token}"))],
    )
    .await;
    // a newline cannot be put in a header, but it can in the method name of the JSON body
    w.post(
        json!({"jsonrpc": "2.0", "id": 2, "method": "x\nmcp: status=200 forged"}).to_string(),
        &[("authorization", format!("Bearer {token}"))],
    )
    .await;
    w.post(
        "SECRET-BODY".into(),
        &[(
            "authorization",
            "Bearer abc.def.SECRET-TOKEN-PART".to_string(),
        )],
    )
    .await;
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    for secret in [
        token.as_str(),
        "SECRET-SCHEMA-TEXT",
        "SECRET-BODY",
        "SECRET-TOKEN-PART",
    ] {
        assert!(!log.contains(secret), "{secret} reached the log");
    }
    assert_eq!(
        log.lines().filter(|l| l.contains("forged")).count(),
        1,
        "the forged text stays inside its own line"
    );
    assert!(
        !log.lines().any(|l| l.starts_with("mcp: status=200")),
        "a method name must not start a line"
    );
}

#[tokio::test]
async fn a_modern_refusal_logs_its_reason() {
    let w = world().await;
    let (buf, _guard) = capture_log();
    let (status, _) = w
        .rpc(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list",
                    "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "1999-01-01"}}}))
        .await;
    assert!(status >= 400, "expected a refusal, got {status}");
    let lines = lines_about_mcp(&buf);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains(&format!("status={status}")),
        "{}",
        lines[0]
    );
    assert!(!lines[0].contains("reason=-"), "{}", lines[0]);
}

#[tokio::test]
async fn a_notification_logs_its_method_and_a_refusal_is_a_warning() {
    let w = world().await;
    let (buf, _guard) = capture_log();
    let res = w
        .post(
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
            &[("authorization", format!("Bearer {}", w.token()))],
        )
        .await;
    assert_eq!(res.status(), 202);
    w.post("{}".into(), &[]).await; // no token: refused
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    let mine: Vec<&str> = log.lines().filter(|l| l.contains("mcp: status=")).collect();
    assert_eq!(mine.len(), 2, "{log}");
    assert!(
        mine[0].contains("INFO")
            && mine[0].contains("status=202")
            && mine[0].contains("method=\"notifications/initialized\""),
        "{}",
        mine[0]
    );
    assert!(
        mine[1].contains("WARN") && mine[1].contains("status=401"),
        "{}",
        mine[1]
    );
}
