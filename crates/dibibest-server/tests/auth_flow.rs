mod common;
use axum::body::Body;
use axum::http::{Method, Request, Response, StatusCode};
use axum::Router;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use common::idp::{Answer, Idp};
use common::{app, fixture, send, text};
use dibibest_server::auth::session::{pkce_challenge, sign, verify, LOGIN_COOKIE, SESSION_COOKIE};
use dibibest_server::config::{Oidc, TokenAuth};
use serde_json::{json, Value};
use std::collections::HashMap;
use url::Url;

const SECRET: &str = "0123456789abcdef0123456789abcdef-session";
const PUBLIC: &str = "https://dibibest.codecora.dev";
const CLIENT_SECRET: &str = "s3cret:with/odd chars";

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

struct Setup {
    app: Router,
    idp: Idp,
    _fx: common::Fixture,
}

async fn setup_with(token_auth: TokenAuth, public: Option<&str>, signed_in_config: bool) -> Setup {
    let idp = Idp::start().await;
    let mut fx = fixture();
    fx.config.public_url = public.map(str::to_string);
    if signed_in_config {
        fx.config.oidc = Some(Oidc {
            issuer: idp.issuer(),
            client_id: "dibibest-web".into(),
            client_secret: CLIENT_SECRET.into(),
            session_secret: SECRET.into(),
            token_auth,
        });
    }
    Setup {
        app: app(&fx),
        idp,
        _fx: fx,
    }
}

async fn setup() -> Setup {
    setup_with(TokenAuth::Basic, Some(PUBLIC), true).await
}

fn get(path: &str, cookie: Option<(&str, &str)>) -> Request<Body> {
    let mut b = Request::builder().uri(path);
    if let Some((k, v)) = cookie {
        b = b.header("cookie", format!("{k}={v}"));
    }
    b.body(Body::empty()).unwrap()
}

/// The full `Set-Cookie` line for `name`, if the response sets it.
fn set_cookie(res: &Response<Body>, name: &str) -> Option<String> {
    res.headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with(&format!("{name}=")))
        .map(str::to_string)
}

fn cookie_value(line: &str) -> String {
    line.split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1
        .to_string()
}

fn header(res: &Response<Body>, name: &str) -> String {
    res.headers()
        .get(name)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default()
}

fn query_of(location: &str) -> HashMap<String, String> {
    Url::parse(location)
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// A login: the redirect, the handshake cookie, and what was in it.
struct Login {
    state: String,
    handshake: String,
    verifier: String,
}

async fn login(s: &Setup, return_to: Option<&str>) -> Login {
    let path = match return_to {
        Some(r) => format!(
            "/auth/login?return_to={}",
            url::form_urlencoded::byte_serialize(r.as_bytes()).collect::<String>()
        ),
        None => "/auth/login".to_string(),
    };
    let res = send(&s.app, get(&path, None)).await;
    assert_eq!(
        res.status(),
        StatusCode::FOUND,
        "login answered {}",
        res.status()
    );
    let state = query_of(&header(&res, "location"))["state"].clone();
    let handshake = cookie_value(&set_cookie(&res, LOGIN_COOKIE).expect("a handshake cookie"));
    let verifier = verify::<Value>(SECRET, &handshake, now()).unwrap()["verifier"]
        .as_str()
        .unwrap()
        .to_string();
    Login {
        state,
        handshake,
        verifier,
    }
}

async fn callback(s: &Setup, l: &Login, code: &str) -> Response<Body> {
    send(
        &s.app,
        get(
            &format!("/auth/callback?code={code}&state={}", l.state),
            Some((LOGIN_COOKIE, &l.handshake)),
        ),
    )
    .await
}

async fn signed_in(s: &Setup) -> String {
    let l = login(s, None).await;
    let res = callback(s, &l, "c-1").await;
    assert_eq!(res.status(), StatusCode::FOUND);
    cookie_value(&set_cookie(&res, SESSION_COOKIE).expect("a session cookie"))
}

fn assert_no_session(res: &Response<Body>) {
    assert!(
        set_cookie(res, SESSION_COOKIE).is_none_or(|l| l.contains("Max-Age=0")),
        "a session was set"
    );
}

fn assert_login_cleared(res: &Response<Body>) {
    let c = set_cookie(res, LOGIN_COOKIE).expect("the handshake cookie is cleared");
    assert!(c.contains("Max-Age=0") && c.contains("Path=/auth"), "{c}");
}

// ── when sign-in is off ────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn with_sign_in_off_me_says_so_and_login_and_callback_are_404() {
    let s = setup_with(TokenAuth::Basic, None, false).await;
    let (status, body) = text(send(&s.app, get("/auth/me", None)).await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({"enabled": false, "user": null})
    );
    for path in ["/auth/login", "/auth/callback?code=c&state=s"] {
        let res = send(&s.app, get(path, None)).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path}");
        assert!(header(&res, "content-type").contains("json"));
    }
}

// ── /auth/me ───────────────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn me_reads_the_session_cookie_and_nothing_else() {
    let s = setup().await;
    let me = |cookie: Option<String>| {
        let s = &s;
        async move {
            let req = match &cookie {
                Some(c) => get("/auth/me", Some((SESSION_COOKIE, c))),
                None => get("/auth/me", None),
            };
            let res = send(&s.app, req).await;
            assert_eq!(header(&res, "cache-control"), "no-store");
            serde_json::from_str::<Value>(&text(res).await.1).unwrap()
        }
    };
    assert_eq!(me(None).await, json!({"enabled": true, "user": null}));
    let good = sign(
        SECRET,
        &json!({"sub": "u1", "email": "a@b.c", "name": "Ana", "exp": now() + 60}),
    );
    assert_eq!(
        me(Some(good.clone())).await,
        json!({"enabled": true, "user": {"sub": "u1", "email": "a@b.c", "name": "Ana"}})
    );
    let no_name = sign(
        SECRET,
        &json!({"sub": "u1", "email": "a@b.c", "exp": now() + 60}),
    );
    assert_eq!(
        me(Some(no_name)).await["user"],
        json!({"sub": "u1", "email": "a@b.c"})
    );
    let (body, mac) = good.split_once('.').unwrap();
    let forged = format!(
        "{}.{mac}",
        dibibest_server::auth::codec::b64url(
            br#"{"sub":"admin","email":"a@b.c","exp":9999999999}"#
        )
    );
    for bad in [
        forged,
        sign(
            "another-secret-of-sufficient-length!!",
            &json!({"sub": "u1", "email": "a@b.c", "exp": now() + 60}),
        ),
        sign(
            SECRET,
            &json!({"sub": "u1", "email": "a@b.c", "exp": now() - 1}),
        ),
        format!("{body}.{mac}.x"),
        "garbage".to_string(),
    ] {
        assert_eq!(me(Some(bad)).await["user"], Value::Null);
    }
}

// ── /auth/login ────────────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn login_redirects_to_the_provider_with_pkce_and_a_state_kept_in_a_cookie() {
    let s = setup().await;
    let res = send(&s.app, get("/auth/login?return_to=/p/abc", None)).await;
    assert_eq!(res.status(), StatusCode::FOUND);
    assert_eq!(header(&res, "cache-control"), "no-store");
    let url = Url::parse(&header(&res, "location")).unwrap();
    assert_eq!(
        format!("{}{}", url.origin().ascii_serialization(), url.path()),
        format!("{}/oauth2/authorize", s.idp.issuer())
    );
    let q = query_of(url.as_str());
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["client_id"], "dibibest-web");
    assert_eq!(q["redirect_uri"], format!("{PUBLIC}/auth/callback"));
    assert_eq!(q["scope"], "openid email profile");
    assert_eq!(q["code_challenge_method"], "S256");
    let cookie = set_cookie(&res, LOGIN_COOKIE).unwrap();
    for part in [
        "HttpOnly",
        "Secure",
        "SameSite=Lax",
        "Path=/auth",
        "Max-Age=600",
    ] {
        assert!(cookie.contains(part), "{part} missing: {cookie}");
    }
    let hs = verify::<Value>(SECRET, &cookie_value(&cookie), now()).expect("a signed handshake");
    assert_eq!(hs["state"], q["state"].as_str());
    assert_eq!(hs["returnTo"], "/p/abc");
    assert_eq!(
        pkce_challenge(hs["verifier"].as_str().unwrap()),
        q["code_challenge"]
    );
}

#[tokio::test]
async fn every_login_has_its_own_state_and_verifier_and_discovery_is_asked_once() {
    let s = setup().await;
    let (a, b) = (login(&s, None).await, login(&s, None).await);
    assert_ne!(a.state, b.state);
    assert_ne!(a.verifier, b.verifier);
    assert!(
        a.verifier.len() >= 43,
        "a PKCE verifier has at least 43 characters"
    );
    assert_eq!(s.idp.calls_to("/.well-known/openid-configuration").len(), 1);
}

#[tokio::test]
async fn a_return_to_that_points_off_site_is_ignored() {
    let s = setup().await;
    for bad in [
        "//evil.test",
        "https://evil.test",
        "/..//evil.test",
        "/\\evil.test",
        "/a/..//evil.test",
    ] {
        let l = login(&s, Some(bad)).await;
        let hs = verify::<Value>(SECRET, &l.handshake, now()).unwrap();
        assert_eq!(hs["returnTo"], "/app", "{bad}");
    }
}

#[tokio::test]
async fn login_says_so_when_the_provider_cannot_be_reached() {
    let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = dead.local_addr().unwrap();
    drop(dead);
    let mut fx = fixture();
    fx.config.public_url = Some(PUBLIC.into());
    fx.config.oidc = Some(Oidc {
        issuer: format!("http://{addr}/api/auth"),
        client_id: "id".into(),
        client_secret: "secret".into(),
        session_secret: SECRET.into(),
        token_auth: TokenAuth::Basic,
    });
    let res = send(&app(&fx), get("/auth/login", None)).await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(header(&res, "content-type").starts_with("text/html"));
    assert!(text(res).await.1.contains("cannot be reached"));
}

// ── /auth/callback ─────────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_whole_flow_signs_a_person_in() {
    let s = setup().await;
    let l = login(&s, Some("/p/abc")).await;
    let res = callback(&s, &l, "c-1").await;
    assert_eq!(res.status(), StatusCode::FOUND);
    assert_eq!(header(&res, "location"), "/p/abc");
    assert_eq!(header(&res, "cache-control"), "no-store");

    let token_call = s
        .idp
        .calls_to("/oauth2/token")
        .pop()
        .expect("the code was exchanged");
    assert_eq!(token_call.method, "POST");
    assert_eq!(
        token_call.headers["content-type"],
        "application/x-www-form-urlencoded"
    );
    let form: HashMap<String, String> = url::form_urlencoded::parse(token_call.body.as_bytes())
        .into_owned()
        .collect();
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["code"], "c-1");
    assert_eq!(form["redirect_uri"], format!("{PUBLIC}/auth/callback"));
    assert_eq!(form["code_verifier"], l.verifier);
    assert!(
        !form.contains_key("client_secret"),
        "basic auth keeps the secret out of the body"
    );
    let basic = STANDARD
        .decode(
            token_call.headers["authorization"]
                .strip_prefix("Basic ")
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        String::from_utf8(basic).unwrap(),
        "dibibest-web:s3cret%3Awith%2Fodd%20chars",
        "RFC 6749 2.3.1 form-encoding"
    );
    assert_eq!(
        s.idp.calls_to("/oauth2/userinfo").pop().unwrap().headers["authorization"],
        "Bearer at-1"
    );

    let session = set_cookie(&res, SESSION_COOKIE).unwrap();
    for part in [
        "HttpOnly",
        "Secure",
        "SameSite=Lax",
        "Path=/",
        "Max-Age=604800",
    ] {
        assert!(session.contains(part), "{part} missing: {session}");
    }
    assert_login_cleared(&res);
    let me = text(
        send(
            &s.app,
            get("/auth/me", Some((SESSION_COOKIE, &cookie_value(&session)))),
        )
        .await,
    )
    .await
    .1;
    assert_eq!(
        serde_json::from_str::<Value>(&me).unwrap(),
        json!({"enabled": true, "user": {"sub": "u1", "email": "ana@example.com"}})
    );
}

#[tokio::test]
async fn the_name_from_the_profile_is_kept_and_an_absent_email_verified_is_accepted() {
    let s = setup().await;
    s.idp.set(
        "userinfo",
        Answer::ok(json!({"sub": "u2", "email": "X@Y.Z", "name": "Xavier"})),
    );
    let session = signed_in(&s).await;
    let payload = verify::<Value>(SECRET, &session, now()).unwrap();
    assert_eq!(payload["name"], "Xavier");
    assert_eq!(payload["email"], "x@y.z", "lower-cased");
}

#[tokio::test]
async fn client_secret_post_when_configured() {
    let s = setup_with(TokenAuth::Post, Some(PUBLIC), true).await;
    let l = login(&s, None).await;
    assert_eq!(callback(&s, &l, "c").await.status(), StatusCode::FOUND);
    let call = s.idp.calls_to("/oauth2/token").pop().unwrap();
    assert!(!call.headers.contains_key("authorization"));
    let form: HashMap<String, String> = url::form_urlencoded::parse(call.body.as_bytes())
        .into_owned()
        .collect();
    assert_eq!(form["client_id"], "dibibest-web");
    assert_eq!(form["client_secret"], CLIENT_SECRET);
}

#[tokio::test]
async fn a_callback_that_does_not_match_a_login_is_refused_and_signs_nobody_in() {
    let s = setup().await;
    let a = login(&s, None).await;
    let b = login(&s, None).await;
    let other_secret = sign(
        "another-secret-of-sufficient-length!!",
        &json!({"state": a.state, "verifier": "v", "returnTo": "/app", "exp": now() + 60}),
    );
    let expired = sign(
        SECRET,
        &json!({"state": a.state, "verifier": "v", "returnTo": "/app", "exp": now() - 1}),
    );
    let cases: Vec<(&str, Request<Body>)> = vec![
        (
            "the state of another login",
            get(
                &format!("/auth/callback?code=c&state={}", b.state),
                Some((LOGIN_COOKIE, &a.handshake)),
            ),
        ),
        (
            "a wrong state",
            get(
                "/auth/callback?code=c&state=nope",
                Some((LOGIN_COOKIE, &a.handshake)),
            ),
        ),
        (
            "no state",
            get("/auth/callback?code=c", Some((LOGIN_COOKIE, &a.handshake))),
        ),
        (
            "no handshake cookie",
            get(&format!("/auth/callback?code=c&state={}", a.state), None),
        ),
        (
            "a handshake signed with another secret",
            get(
                &format!("/auth/callback?code=c&state={}", a.state),
                Some((LOGIN_COOKIE, &other_secret)),
            ),
        ),
        (
            "an expired handshake",
            get(
                &format!("/auth/callback?code=c&state={}", a.state),
                Some((LOGIN_COOKIE, &expired)),
            ),
        ),
        (
            "no code",
            get(
                &format!("/auth/callback?state={}", a.state),
                Some((LOGIN_COOKIE, &a.handshake)),
            ),
        ),
    ];
    for (name, req) in cases {
        let res = send(&s.app, req).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{name}");
        assert_no_session(&res);
        assert_login_cleared(&res);
    }
    assert!(
        s.idp.calls_to("/oauth2/token").is_empty(),
        "no code was ever exchanged"
    );
}

#[tokio::test]
async fn a_cancelled_sign_in_says_so_and_does_not_repeat_what_it_was_told() {
    let s = setup().await;
    let res = send(&s.app, get("/auth/callback?error=access_denied", None)).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_login_cleared(&res);
    let body = text(res).await.1;
    assert!(body.contains("cancelled") && body.contains("access_denied"));
    // a crafted link must not put its own words, or markup, on a page that looks like ours
    let res = send(
        &s.app,
        get(
            "/auth/callback?error=%3Cscript%3Ealert(1)%3C/script%3E%20Call%20us%20on%200800",
            None,
        ),
    )
    .await;
    let body = text(res).await.1;
    assert!(
        !body.contains("<script>") && !body.contains("Call us"),
        "{body}"
    );
}

#[tokio::test]
async fn the_error_pages_are_locked_down() {
    let s = setup().await;
    let res = send(&s.app, get("/auth/callback?error=x", None)).await;
    assert!(header(&res, "content-type").starts_with("text/html"));
    assert_eq!(header(&res, "cache-control"), "no-store");
    assert_eq!(header(&res, "x-content-type-options"), "nosniff");
    let csp = header(&res, "content-security-policy");
    assert!(
        csp.contains("default-src 'none'") && !csp.contains("script-src"),
        "{csp}"
    );
}

#[tokio::test]
async fn a_profile_that_is_not_good_enough_does_not_sign_anyone_in() {
    for (profile, status) in [
        (
            json!({"sub": "u", "email": "a@b.c", "email_verified": false}),
            StatusCode::FORBIDDEN,
        ),
        (json!({"sub": "u"}), StatusCode::BAD_GATEWAY),
        (json!({"email": "a@b.c"}), StatusCode::BAD_GATEWAY),
        (
            json!({"sub": "", "email": "a@b.c"}),
            StatusCode::BAD_GATEWAY,
        ),
        (json!({"sub": "u", "email": ""}), StatusCode::BAD_GATEWAY),
    ] {
        let s = setup().await;
        s.idp.set("userinfo", Answer::ok(profile.clone()));
        let l = login(&s, None).await;
        let res = callback(&s, &l, "c").await;
        assert_eq!(res.status(), status, "{profile}");
        assert_no_session(&res);
        assert_login_cleared(&res);
    }
}

#[tokio::test]
async fn a_provider_that_misbehaves_during_the_exchange_gives_a_clean_failure() {
    let html = |status| Answer {
        status,
        raw: Some("<html>proxy error</html>".into()),
        ..Answer::ok(json!({}))
    };
    let cases: Vec<(&str, &str, Answer, StatusCode)> = vec![
        (
            "token refused",
            "token",
            Answer {
                status: 400,
                ..Answer::ok(json!({"error": "invalid_grant"}))
            },
            StatusCode::BAD_GATEWAY,
        ),
        (
            "token answer is not JSON",
            "token",
            html(200),
            StatusCode::BAD_GATEWAY,
        ),
        (
            "token answer has no access token",
            "token",
            Answer::ok(json!({"token_type": "Bearer"})),
            StatusCode::BAD_GATEWAY,
        ),
        (
            "token answer has an empty access token",
            "token",
            Answer::ok(json!({"access_token": ""})),
            StatusCode::BAD_GATEWAY,
        ),
        (
            "token answer is a redirect",
            "token",
            Answer {
                status: 302,
                location: Some("http://127.0.0.1:1/x".into()),
                ..Answer::ok(json!({}))
            },
            StatusCode::BAD_GATEWAY,
        ),
        (
            "userinfo refused",
            "userinfo",
            Answer {
                status: 401,
                ..Answer::ok(json!({}))
            },
            StatusCode::BAD_GATEWAY,
        ),
        (
            "userinfo answer is not JSON",
            "userinfo",
            html(200),
            StatusCode::BAD_GATEWAY,
        ),
    ];
    for (name, endpoint, answer, status) in cases {
        let s = setup().await;
        s.idp.set(endpoint, answer);
        let l = login(&s, None).await;
        let res = callback(&s, &l, "c").await;
        assert_eq!(res.status(), status, "{name}");
        assert!(
            header(&res, "content-type").starts_with("text/html"),
            "{name}"
        );
        assert_no_session(&res);
        assert_login_cleared(&res);
    }
}

#[tokio::test]
async fn a_refused_exchange_says_which_step_failed_and_with_what_status() {
    for (endpoint, status, expect) in [
        ("token", 400, "Token exchange failed (400)"),
        ("userinfo", 401, "Could not read your profile (401)"),
    ] {
        let s = setup().await;
        s.idp.set(
            endpoint,
            Answer {
                status,
                ..Answer::ok(json!({"error": "x"}))
            },
        );
        let l = login(&s, None).await;
        let res = callback(&s, &l, "c").await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        let body = text(res).await.1;
        assert!(body.contains(expect), "{endpoint}: {body}");
    }
}

#[tokio::test]
async fn a_token_or_userinfo_endpoint_that_cannot_be_reached_is_unavailable() {
    for endpoint in ["token_endpoint", "userinfo_endpoint"] {
        let s = setup().await;
        let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = dead.local_addr().unwrap();
        drop(dead);
        let base = s.idp.issuer();
        let mut doc = json!({
            "authorization_endpoint": format!("{base}/oauth2/authorize"),
            "token_endpoint": format!("{base}/oauth2/token"),
            "userinfo_endpoint": format!("{base}/oauth2/userinfo"),
        });
        doc[endpoint] = json!(format!("http://{addr}/gone"));
        s.idp.set("discovery", Answer::ok(doc));
        let l = login(&s, None).await;
        let res = callback(&s, &l, "c").await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE, "{endpoint}");
        assert_no_session(&res);
    }
}

#[tokio::test]
async fn the_return_to_in_the_cookie_is_cleaned_again_before_it_is_used() {
    // A handshake cookie that is correctly signed but holds an address that goes off-site, as one issued before
    // the open redirect was fixed could.
    let s = setup().await;
    let l = login(&s, None).await;
    let old = sign(
        SECRET,
        &json!({"state": l.state, "verifier": l.verifier, "returnTo": "//evil.test", "exp": now() + 60}),
    );
    let res = send(
        &s.app,
        get(
            &format!("/auth/callback?code=c&state={}", l.state),
            Some((LOGIN_COOKIE, &old)),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::FOUND);
    assert_eq!(header(&res, "location"), "/app");
}

#[tokio::test]
async fn cookies_are_not_marked_secure_on_a_plain_http_public_address() {
    let s = setup_with(TokenAuth::Basic, Some("http://localhost:8080"), true).await;
    let res = send(&s.app, get("/auth/login", None)).await;
    let cookie = set_cookie(&res, LOGIN_COOKIE).unwrap();
    assert!(
        !cookie.contains("Secure") && cookie.contains("HttpOnly"),
        "{cookie}"
    );
    assert_eq!(
        query_of(&header(&res, "location"))["redirect_uri"],
        "http://localhost:8080/auth/callback"
    );
}

// ── /auth/logout ───────────────────────────────────────────────────────────────────────────────────────

fn post(path: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut b = Request::builder().method(Method::POST).uri(path);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    b.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn logout_is_post_only_and_same_origin_only() {
    let s = setup().await;
    let res = send(&s.app, get("/auth/logout", None)).await;
    assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert!(header(&res, "allow").contains("POST"));
    for headers in [
        vec![],
        vec![("origin", "https://evil.test")],
        vec![("sec-fetch-site", "cross-site")],
        vec![("origin", "null")],
    ] {
        assert_eq!(
            send(&s.app, post("/auth/logout", &headers)).await.status(),
            StatusCode::FORBIDDEN,
            "{headers:?}"
        );
    }
    for headers in [
        vec![("origin", PUBLIC)],
        vec![("sec-fetch-site", "same-origin")],
    ] {
        let res = send(&s.app, post("/auth/logout", &headers)).await;
        assert_eq!(res.status(), StatusCode::SEE_OTHER, "{headers:?}");
        assert_eq!(header(&res, "location"), "/");
        let c = set_cookie(&res, SESSION_COOKIE).unwrap();
        assert!(
            c.contains("Max-Age=0") && c.starts_with(&format!("{SESSION_COOKIE}=;")),
            "{c}"
        );
    }
}
