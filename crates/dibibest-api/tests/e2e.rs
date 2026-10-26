//! End-to-end: real axum servers on ephemeral ports, in-process mock OpenID Provider.

use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::Redirect,
    routing::{get, post},
    Form, Json, Router,
};
use dibibest_api::{app, config::Oidc, session, store::Store};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::net::TcpListener;

const CID: &str = "test-client";
const CSEC: &str = "test-client-secret";
const SESSION_SECRET: &str = "test-session-secret-0123456789abcdef";

struct Op {
    base: String,
    email: String,
    verified: Option<bool>,
    issuer: Option<String>,
    codes: Mutex<HashMap<String, String>>, // code -> challenge
}

async fn serve(l: TcpListener, r: Router) {
    tokio::spawn(async move { axum::serve(l, r).await.unwrap() });
}

async fn spawn_op(email: &str) -> String {
    spawn_op_with(email, None, None).await
}

async fn spawn_op_with(email: &str, verified: Option<bool>, issuer: Option<&str>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let op = Arc::new(Op {
        base: base.clone(),
        email: email.into(),
        verified,
        issuer: issuer.map(Into::into),
        codes: Default::default(),
    });
    let r = Router::new()
        .route(
            "/.well-known/openid-configuration",
            get(|State(o): State<Arc<Op>>| async move {
                Json(json!({
                    "issuer": o.issuer.clone().unwrap_or(o.base.clone()),
                    "authorization_endpoint": format!("{}/authorize", o.base),
                    "token_endpoint": format!("{}/token", o.base),
                    "userinfo_endpoint": format!("{}/userinfo", o.base),
                }))
            }),
        )
        .route(
            "/authorize",
            get(
                |State(o): State<Arc<Op>>, Query(q): Query<HashMap<String, String>>| async move {
                    assert_eq!(q["code_challenge_method"], "S256");
                    assert_eq!(q["client_id"], CID);
                    let code = session::random_token(8);
                    o.codes
                        .lock()
                        .unwrap()
                        .insert(code.clone(), q["code_challenge"].clone());
                    Redirect::to(&format!(
                        "{}?code={code}&state={}",
                        q["redirect_uri"], q["state"]
                    ))
                },
            ),
        )
        .route(
            "/token",
            post(
                |State(o): State<Arc<Op>>, Form(f): Form<HashMap<String, String>>| async move {
                    let bad = |m: &str| (StatusCode::BAD_REQUEST, Json(json!({ "error": m })));
                    if f.get("client_id").map(String::as_str) != Some(CID)
                        || f.get("client_secret").map(String::as_str) != Some(CSEC)
                    {
                        return Err(bad("invalid_client"));
                    }
                    let challenge = o
                        .codes
                        .lock()
                        .unwrap()
                        .remove(f.get("code").map_or("", String::as_str))
                        .ok_or_else(|| bad("invalid_grant"))?;
                    let verifier = f
                        .get("code_verifier")
                        .ok_or_else(|| bad("missing verifier"))?;
                    if session::pkce_challenge(verifier) != challenge {
                        return Err(bad("bad verifier"));
                    }
                    Ok(Json(
                        json!({ "access_token": "at-good", "token_type": "Bearer" }),
                    ))
                },
            ),
        )
        .route(
            "/userinfo",
            get(|State(o): State<Arc<Op>>, h: HeaderMap| async move {
                if h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok())
                    != Some("Bearer at-good")
                {
                    return Err(StatusCode::UNAUTHORIZED);
                }
                let mut u = json!({ "sub": "u1", "email": o.email });
                if let Some(v) = o.verified {
                    u["email_verified"] = json!(v);
                }
                Ok(Json(u))
            }),
        )
        .with_state(op);
    serve(l, r).await;
    base
}

async fn spawn_api(issuer: Option<&str>, allow: &str) -> String {
    spawn_api_db(issuer, allow, ":memory:").await
}

async fn spawn_api_db(issuer: Option<&str>, allow: &str, db: &str) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let oidc = issuer.map(|i| {
        let env: HashMap<&str, String> = HashMap::from([
            ("OIDC_ISSUER", i.to_string()),
            ("OIDC_CLIENT_ID", CID.into()),
            ("OIDC_CLIENT_SECRET", CSEC.into()),
            ("PUBLIC_URL", base.clone()),
            ("SESSION_SECRET", SESSION_SECRET.into()),
            ("ALLOWED_EMAILS", allow.into()),
        ]);
        Oidc::from_lookup(|k| env.get(k).cloned()).unwrap()
    });
    serve(
        l,
        app::router(app::AppState::new(oidc, Store::open(db).unwrap())),
    )
    .await;
    base
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

fn set_cookies(r: &reqwest::Response) -> Vec<String> {
    r.headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect()
}

fn pair(c: &str) -> String {
    c.split(';').next().unwrap().to_string()
}

/// Walk login -> OP authorize -> callback. Returns the callback response.
async fn sign_in(api: &str) -> reqwest::Response {
    let c = client();
    let r = c.get(format!("{api}/auth/login")).send().await.unwrap();
    assert_eq!(r.status(), 303);
    let pkce = set_cookies(&r)
        .into_iter()
        .find(|c| c.starts_with("dibibest_pkce="))
        .unwrap();
    assert!(pkce.contains("HttpOnly"));
    let authz = r.headers()[header::LOCATION].to_str().unwrap().to_string();
    let r = c.get(&authz).send().await.unwrap();
    assert_eq!(r.status(), 303);
    let cb = r.headers()[header::LOCATION].to_str().unwrap().to_string();
    assert!(cb.starts_with(&format!("{api}/auth/callback?")));
    c.get(cb)
        .header(header::COOKIE, pair(&pkce))
        .send()
        .await
        .unwrap()
}

fn forged(email: &str) -> String {
    let tok = session::sign(
        &session::derive_key(SESSION_SECRET),
        &json!({ "email": email, "exp": session::now() + 60 }),
    );
    format!("dibibest_session={tok}")
}

#[tokio::test]
async fn full_login_then_projects() {
    let op = spawn_op("Alice@Example.com").await;
    let api = spawn_api(Some(&op), "alice@example.com").await;
    let c = client();

    let st: Value = c
        .get(format!("{api}/auth/oidc/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(st, json!({ "enabled": true }));
    assert_eq!(c.get(format!("{api}/")).send().await.unwrap().status(), 200);

    let r = sign_in(&api).await;
    assert_eq!(r.status(), 303);
    assert_eq!(r.headers()[header::LOCATION], "/app");
    let cookies = set_cookies(&r);
    let sess = cookies
        .iter()
        .find(|c| c.starts_with("dibibest_session="))
        .expect("session cookie");
    assert!(sess.contains("HttpOnly") && sess.contains("SameSite=Lax") && sess.contains("Path=/"));
    let cookie = pair(sess);

    let url = format!("{api}/api/projects");
    let body = json!({ "name": "p1", "erdjson": { "tables": [] } });
    assert_eq!(c.post(&url).json(&body).send().await.unwrap().status(), 401);
    let r = c
        .post(&url)
        .header(header::COOKIE, &cookie)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let loc = r.headers()[header::LOCATION].to_str().unwrap().to_string();
    let p: Value = r.json().await.unwrap();
    assert_eq!(p["name"], "p1");
    assert_eq!(loc, format!("/api/projects/{}", p["id"].as_str().unwrap()));
    let r = c
        .get(&url)
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()[header::CACHE_CONTROL], "no-store");

    // signed-in `/` redirects to /app
    let r = c
        .get(format!("{api}/"))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 303);
    assert_eq!(r.headers()[header::LOCATION], "/app");

    // logout clears the cookie
    let r = c.post(format!("{api}/auth/logout")).send().await.unwrap();
    assert!(set_cookies(&r)[0].contains("Max-Age=0"));
}

#[tokio::test]
async fn allowlist_denied_gets_page_and_no_session() {
    let op = spawn_op("mallory@example.com").await;
    let api = spawn_api(Some(&op), "alice@example.com").await;
    let r = sign_in(&api).await;
    assert_eq!(r.status(), 403);
    assert!(!set_cookies(&r)
        .iter()
        .any(|c| c.starts_with("dibibest_session=") && !c.contains("Max-Age=0")));
    let page = r.text().await.unwrap();
    assert!(page.contains("Not authorized") && page.contains("/auth/logout"));
}

#[tokio::test]
async fn callback_rejects_bad_state_missing_cookie_and_bad_secret() {
    let op = spawn_op("a@example.com").await;
    let api = spawn_api(Some(&op), "").await;
    let c = client();
    let r = c
        .get(format!("{api}/auth/callback?code=x&state=y"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let r = c.get(format!("{api}/auth/login")).send().await.unwrap();
    let pkce = pair(&set_cookies(&r)[0]);
    let r = c
        .get(format!("{api}/auth/callback?code=x&state=wrong"))
        .header(header::COOKIE, &pkce)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    // right state but unknown code -> OP rejects -> 502, no session
    let authz = c.get(format!("{api}/auth/login")).send().await.unwrap();
    let pkce = pair(&set_cookies(&authz)[0]);
    let loc = authz.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_string();
    let state = reqwest::Url::parse(&loc)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .to_string();
    let r = c
        .get(format!("{api}/auth/callback?code=bogus&state={state}"))
        .header(header::COOKIE, pkce)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    assert!(set_cookies(&r).is_empty());
}

#[tokio::test]
async fn feature_off_is_clean() {
    let api = spawn_api(None, "").await;
    let c = client();
    let st: Value = c
        .get(format!("{api}/auth/oidc/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(st, json!({ "enabled": false }));
    for p in ["/", "/auth/login", "/api/projects"] {
        let r = c.get(format!("{api}{p}")).send().await.unwrap();
        assert_eq!(r.status(), 404, "{p}");
        assert!(r.json::<Value>().await.unwrap()["error"].is_string());
    }
}

#[tokio::test]
async fn projects_crud_scoping_and_caps() {
    let api = spawn_api(Some("http://127.0.0.1:1"), "").await;
    let c = client();
    let (a, b) = (forged("a@example.com"), forged("b@example.com"));
    let url = format!("{api}/api/projects");
    let send = |m: reqwest::Method, u: String, ck: &str, body: Option<Value>| {
        let mut r = c.request(m, u).header(header::COOKIE, ck.to_string());
        if let Some(b) = body {
            r = r.json(&b);
        }
        r.send()
    };

    let r = send(
        "POST".parse().unwrap(),
        url.clone(),
        &a,
        Some(json!({ "name": "one", "erdjson": { "v": 1 } })),
    )
    .await
    .unwrap();
    let id = r.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let one = format!("{url}/{id}");

    let l: Value = send("GET".parse().unwrap(), url.clone(), &a, None)
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(l["projects"].as_array().unwrap().len(), 1);
    let g: Value = send("GET".parse().unwrap(), one.clone(), &a, None)
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(g["erdjson"], json!({ "v": 1 }));

    // owner scoping
    for m in ["GET", "DELETE"] {
        assert_eq!(
            send(m.parse().unwrap(), one.clone(), &b, None)
                .await
                .unwrap()
                .status(),
            404
        );
    }
    assert_eq!(
        send(
            "PUT".parse().unwrap(),
            one.clone(),
            &b,
            Some(json!({ "name": "x", "erdjson": 1 }))
        )
        .await
        .unwrap()
        .status(),
        404
    );
    let lb: Value = send("GET".parse().unwrap(), url.clone(), &b, None)
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(lb["projects"].as_array().unwrap().is_empty());

    // update
    let u: Value = send(
        "PUT".parse().unwrap(),
        one.clone(),
        &a,
        Some(json!({ "name": "renamed", "erdjson": { "v": 2 } })),
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(
        (u["name"].as_str(), &u["erdjson"]),
        (Some("renamed"), &json!({ "v": 2 }))
    );

    // validation + size cap
    assert_eq!(
        send(
            "POST".parse().unwrap(),
            url.clone(),
            &a,
            Some(json!({ "name": "", "erdjson": 1 }))
        )
        .await
        .unwrap()
        .status(),
        400
    );
    let big = "x".repeat(512 * 1024 + 1);
    assert_eq!(
        send(
            "POST".parse().unwrap(),
            url.clone(),
            &a,
            Some(json!({ "name": "big", "erdjson": big }))
        )
        .await
        .unwrap()
        .status(),
        413
    );

    // delete, then per-user cap (99 more fills the 100 of b; 101st -> 409)
    assert_eq!(
        send("DELETE".parse().unwrap(), one.clone(), &a, None)
            .await
            .unwrap()
            .status(),
        204
    );
    assert_eq!(
        send("GET".parse().unwrap(), one, &a, None)
            .await
            .unwrap()
            .status(),
        404
    );
    for i in 0..100 {
        let r = send(
            "POST".parse().unwrap(),
            url.clone(),
            &b,
            Some(json!({ "name": format!("p{i}"), "erdjson": {} })),
        )
        .await
        .unwrap();
        assert_eq!(r.status(), 201);
    }
    let r = send(
        "POST".parse().unwrap(),
        url.clone(),
        &b,
        Some(json!({ "name": "over", "erdjson": {} })),
    )
    .await
    .unwrap();
    assert_eq!(r.status(), 409);
    // other users unaffected
    assert_eq!(
        send(
            "POST".parse().unwrap(),
            url,
            &a,
            Some(json!({ "name": "ok", "erdjson": {} }))
        )
        .await
        .unwrap()
        .status(),
        201
    );
}

#[tokio::test]
async fn unverified_email_denied_and_no_session() {
    let op = spawn_op_with("a@example.com", Some(false), None).await;
    let api = spawn_api(Some(&op), "").await;
    let r = sign_in(&api).await;
    assert_eq!(r.status(), 403);
    assert!(!set_cookies(&r)
        .iter()
        .any(|c| c.starts_with("dibibest_session=")));
    assert!(r.text().await.unwrap().contains("Not authorized"));
}

#[tokio::test]
async fn verified_email_true_is_allowed() {
    let op = spawn_op_with("a@example.com", Some(true), None).await;
    let api = spawn_api(Some(&op), "").await;
    assert_eq!(sign_in(&api).await.status(), 303);
}

#[tokio::test]
async fn discovery_issuer_mismatch_is_bad_gateway() {
    let op = spawn_op_with("a@example.com", None, Some("https://evil.example")).await;
    let api = spawn_api(Some(&op), "").await;
    let r = client()
        .get(format!("{api}/auth/login"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    assert!(set_cookies(&r).is_empty());
}

#[tokio::test]
async fn removed_from_allowlist_loses_access() {
    let wide = spawn_api(Some("http://127.0.0.1:1"), "").await;
    let narrow = spawn_api(Some("http://127.0.0.1:1"), "someone-else@example.com").await;
    let ck = forged("a@example.com");
    for (api, want) in [(&wide, 200), (&narrow, 401)] {
        let r = client()
            .get(format!("{api}/api/projects"))
            .header(header::COOKIE, &ck)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), want, "{api}");
    }
}

#[tokio::test]
async fn corrupt_stored_document_is_500() {
    let db = std::env::temp_dir().join(format!("dibibest-e2e-{}.db", session::random_token(6)));
    let path = db.to_str().unwrap();
    let api = spawn_api_db(Some("http://127.0.0.1:1"), "", path).await;
    let (c, ck) = (client(), forged("a@example.com"));
    let r = c
        .post(format!("{api}/api/projects"))
        .header(header::COOKIE, &ck)
        .json(&json!({ "name": "p", "erdjson": {} }))
        .send()
        .await
        .unwrap();
    let id = r.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    rusqlite::Connection::open(path)
        .unwrap()
        .execute("UPDATE projects SET erdjson='{not json'", [])
        .unwrap();
    let r = c
        .get(format!("{api}/api/projects/{id}"))
        .header(header::COOKIE, &ck)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 500);
    assert_eq!(
        r.json::<Value>().await.unwrap(),
        json!({ "error": "stored document is corrupt" })
    );
    let _ = std::fs::remove_file(db);
}
