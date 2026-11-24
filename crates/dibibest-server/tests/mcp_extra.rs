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
        Answer::ok(json!({"issuer": 5, "authorization_endpoint": "x"})),
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
