//! axum router: OIDC relying party (`/auth/*`), landing page and saved-projects CRUD.

use crate::config::Oidc;
use crate::session::{derive_key, now, pkce_challenge, random_token, sign, verify};
use crate::store::{CreateErr, Store, MAX_PROJECT_BYTES};
use axum::{
    extract::{DefaultBodyLimit, FromRequestParts, Path, Query, State},
    http::{header, request::Parts, HeaderMap, StatusCode},
    response::{AppendHeaders, IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc, time::Duration};
use subtle::ConstantTimeEq;

const SESSION_COOKIE: &str = "dibibest_session";
const PKCE_COOKIE: &str = "dibibest_pkce";
const SESSION_TTL: u64 = 7 * 24 * 3600;
const PKCE_TTL: u64 = 600;

pub struct AppState {
    pub oidc: Option<Oidc>,
    key: [u8; 32],
    store: Store,
    http: reqwest::Client,
}

impl AppState {
    pub fn new(oidc: Option<Oidc>, store: Store) -> Arc<Self> {
        let key = derive_key(oidc.as_ref().map_or("", |o| &o.session_secret));
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client");
        Arc::new(Self {
            oidc,
            key,
            store,
            http,
        })
    }
}

type S = Arc<AppState>;

pub struct ApiErr(StatusCode, String);

impl ApiErr {
    fn new(c: StatusCode, m: &str) -> Self {
        Self(c, m.to_string())
    }
}

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn db(e: impl std::fmt::Display) -> ApiErr {
    eprintln!("db error: {e}");
    ApiErr::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
}

fn enabled(s: &AppState) -> Result<&Oidc, ApiErr> {
    s.oidc
        .as_ref()
        .ok_or_else(|| ApiErr::new(StatusCode::NOT_FOUND, "auth not configured"))
}

fn cookie_value(h: &HeaderMap, name: &str) -> Option<String> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|p| {
            p.trim()
                .strip_prefix(name)?
                .strip_prefix('=')
                .map(str::to_string)
        })
}

fn set_cookie(o: &Oidc, name: &str, value: &str, max_age: u64) -> String {
    let secure = if o.public_url.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    format!("{name}={value}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}{secure}")
}

fn session_email(s: &AppState, h: &HeaderMap) -> Option<String> {
    s.oidc.as_ref()?;
    let v = verify(&s.key, &cookie_value(h, SESSION_COOKIE)?)?;
    v.get("email")?.as_str().map(str::to_string)
}

/// Extractor: the signed-in email. 404 when auth is off, 401 without a valid session.
pub struct Auth(String);

impl FromRequestParts<S> for Auth {
    type Rejection = ApiErr;
    async fn from_request_parts(p: &mut Parts, s: &S) -> Result<Self, ApiErr> {
        enabled(s)?;
        session_email(s, &p.headers)
            .map(Auth)
            .ok_or_else(|| ApiErr::new(StatusCode::UNAUTHORIZED, "authentication required"))
    }
}

pub fn router(state: S) -> Router {
    Router::new()
        .route("/", get(landing))
        .route("/auth/oidc/status", get(status))
        .route("/auth/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/auth/logout", post(logout))
        .route("/api/projects", get(list).post(create))
        .route("/api/projects/{id}", get(read).put(update).delete(remove))
        .layer(DefaultBodyLimit::max(MAX_PROJECT_BYTES * 2))
        .with_state(state)
}

fn html(body: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><title>dibibest</title><body style=\"font-family:sans-serif;max-width:28rem;margin:4rem auto;padding:1.5rem;border:1px solid #ccc;border-radius:8px\">{body}</body>"
    )
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

async fn status(State(s): State<S>) -> Json<Value> {
    Json(json!({ "enabled": s.oidc.is_some() }))
}

async fn landing(State(s): State<S>, h: HeaderMap) -> Result<Response, ApiErr> {
    enabled(&s)?;
    if session_email(&s, &h).is_some() {
        return Ok(Redirect::to("/app").into_response());
    }
    Ok(axum::response::Html(html(
        "<h1>dibibest</h1><p>Sign in to save your schemas.</p><p><a href=\"/auth/login\">Sign in via CodeCora ID</a></p>",
    ))
    .into_response())
}

async fn discover(s: &AppState, o: &Oidc) -> Result<Value, ApiErr> {
    let bad = |_| ApiErr::new(StatusCode::BAD_GATEWAY, "identity provider unreachable");
    let r = s
        .http
        .get(format!("{}/.well-known/openid-configuration", o.issuer))
        .send()
        .await
        .map_err(bad)?;
    if !r.status().is_success() {
        return Err(ApiErr::new(
            StatusCode::BAD_GATEWAY,
            "identity provider discovery failed",
        ));
    }
    r.json().await.map_err(bad)
}

fn endpoint<'a>(d: &'a Value, k: &str) -> Result<&'a str, ApiErr> {
    d[k].as_str()
        .ok_or_else(|| ApiErr::new(StatusCode::BAD_GATEWAY, "incomplete provider discovery"))
}

async fn login(State(s): State<S>) -> Result<Response, ApiErr> {
    let o = enabled(&s)?;
    let d = discover(&s, o).await?;
    let (state, verifier) = (random_token(24), random_token(32));
    let mut url = reqwest::Url::parse(endpoint(&d, "authorization_endpoint")?)
        .map_err(|_| ApiErr::new(StatusCode::BAD_GATEWAY, "bad authorization endpoint"))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &o.client_id)
        .append_pair("redirect_uri", &format!("{}/auth/callback", o.public_url))
        .append_pair("scope", "openid profile email")
        .append_pair("state", &state)
        .append_pair("code_challenge", &pkce_challenge(&verifier))
        .append_pair("code_challenge_method", "S256");
    let pkce = sign(
        &s.key,
        &json!({ "state": state, "verifier": verifier, "exp": now() + PKCE_TTL }),
    );
    Ok((
        AppendHeaders([(
            header::SET_COOKIE,
            set_cookie(o, PKCE_COOKIE, &pkce, PKCE_TTL),
        )]),
        Redirect::to(url.as_str()),
    )
        .into_response())
}

async fn exchange(
    s: &AppState,
    o: &Oidc,
    d: &Value,
    code: &str,
    verifier: &str,
) -> Result<String, ApiErr> {
    let bad = |m: &'static str| move |_| ApiErr::new(StatusCode::BAD_GATEWAY, m);
    let redirect_uri = format!("{}/auth/callback", o.public_url);
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", &redirect_uri),
        ("client_id", &o.client_id),
        ("client_secret", &o.client_secret),
        ("code_verifier", verifier),
    ];
    let r = s
        .http
        .post(endpoint(d, "token_endpoint")?)
        .form(&form)
        .send()
        .await
        .map_err(bad("token endpoint unreachable"))?;
    if !r.status().is_success() {
        return Err(ApiErr::new(
            StatusCode::BAD_GATEWAY,
            "token exchange rejected",
        ));
    }
    let t: Value = r.json().await.map_err(bad("bad token response"))?;
    let at = t["access_token"]
        .as_str()
        .ok_or_else(|| ApiErr::new(StatusCode::BAD_GATEWAY, "no access token"))?;
    let r = s
        .http
        .get(endpoint(d, "userinfo_endpoint")?)
        .bearer_auth(at)
        .send()
        .await
        .map_err(bad("userinfo unreachable"))?;
    if !r.status().is_success() {
        return Err(ApiErr::new(StatusCode::BAD_GATEWAY, "userinfo rejected"));
    }
    let u: Value = r.json().await.map_err(bad("bad userinfo response"))?;
    u["email"]
        .as_str()
        .filter(|e| !e.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ApiErr::new(StatusCode::BAD_GATEWAY, "identity has no email"))
}

async fn callback(
    State(s): State<S>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, ApiErr> {
    let o = enabled(&s)?;
    if q.contains_key("error") {
        return Err(ApiErr::new(
            StatusCode::BAD_REQUEST,
            "authorization denied by provider",
        ));
    }
    let pkce = cookie_value(&h, PKCE_COOKIE)
        .and_then(|c| verify(&s.key, &c))
        .ok_or_else(|| ApiErr::new(StatusCode::BAD_REQUEST, "login expired, start again"))?;
    let (want, got) = (
        pkce["state"].as_str().unwrap_or(""),
        q.get("state").map_or("", String::as_str),
    );
    if want.is_empty() || !bool::from(want.as_bytes().ct_eq(got.as_bytes())) {
        return Err(ApiErr::new(StatusCode::BAD_REQUEST, "state mismatch"));
    }
    let code = q
        .get("code")
        .ok_or_else(|| ApiErr::new(StatusCode::BAD_REQUEST, "missing code"))?;
    let d = discover(&s, o).await?;
    let email = exchange(&s, o, &d, code, pkce["verifier"].as_str().unwrap_or("")).await?;
    let clear_pkce = set_cookie(o, PKCE_COOKIE, "", 0);
    if !o.allows(&email) {
        let page = html(&format!(
            "<h1>Not authorized</h1><p>{} is not allowed to use this instance.</p><form method=post action=\"/auth/logout\"><button>Log out</button></form>",
            esc(&email)
        ));
        return Ok((
            StatusCode::FORBIDDEN,
            AppendHeaders([(header::SET_COOKIE, clear_pkce)]),
            axum::response::Html(page),
        )
            .into_response());
    }
    let sess = sign(
        &s.key,
        &json!({ "email": email, "exp": now() + SESSION_TTL }),
    );
    Ok((
        AppendHeaders([
            (header::SET_COOKIE, clear_pkce),
            (
                header::SET_COOKIE,
                set_cookie(o, SESSION_COOKIE, &sess, SESSION_TTL),
            ),
        ]),
        Redirect::to("/app"),
    )
        .into_response())
}

async fn logout(State(s): State<S>) -> Result<Response, ApiErr> {
    let o = enabled(&s)?;
    Ok((
        AppendHeaders([(header::SET_COOKIE, set_cookie(o, SESSION_COOKIE, "", 0))]),
        Redirect::to("/"),
    )
        .into_response())
}

fn parse_body(body: &[u8]) -> Result<(String, String), ApiErr> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|_| ApiErr::new(StatusCode::BAD_REQUEST, "invalid JSON body"))?;
    let name = v["name"]
        .as_str()
        .map(str::trim)
        .filter(|n| !n.is_empty() && n.chars().count() <= 200);
    let name =
        name.ok_or_else(|| ApiErr::new(StatusCode::BAD_REQUEST, "name must be 1-200 characters"))?;
    if v.get("erdjson").is_none_or(Value::is_null) {
        return Err(ApiErr::new(StatusCode::BAD_REQUEST, "erdjson is required"));
    }
    let erd = v["erdjson"].to_string();
    if erd.len() > MAX_PROJECT_BYTES {
        return Err(ApiErr::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "project exceeds 512 KB",
        ));
    }
    Ok((name.to_string(), erd))
}

async fn list(State(s): State<S>, Auth(me): Auth) -> Result<Json<Value>, ApiErr> {
    Ok(Json(json!({ "projects": s.store.list(&me).map_err(db)? })))
}

async fn create(
    State(s): State<S>,
    Auth(me): Auth,
    body: axum::body::Bytes,
) -> Result<Response, ApiErr> {
    let (name, erd) = parse_body(&body)?;
    let id = random_token(12);
    match s.store.create(&me, &id, &name, &erd) {
        Ok(()) => {}
        Err(CreateErr::LimitReached) => {
            return Err(ApiErr::new(
                StatusCode::CONFLICT,
                "project limit reached (100)",
            ))
        }
        Err(CreateErr::Db(e)) => return Err(db(e)),
    }
    let p = s.store.get(&me, &id).map_err(db)?;
    Ok((StatusCode::OK, Json(p)).into_response())
}

async fn read(
    State(s): State<S>,
    Auth(me): Auth,
    Path(id): Path<String>,
) -> Result<Response, ApiErr> {
    let p = s
        .store
        .get(&me, &id)
        .map_err(db)?
        .ok_or_else(|| ApiErr::new(StatusCode::NOT_FOUND, "project not found"))?;
    Ok(Json(p).into_response())
}

async fn update(
    State(s): State<S>,
    Auth(me): Auth,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Result<Response, ApiErr> {
    let (name, erd) = parse_body(&body)?;
    if !s.store.update(&me, &id, &name, &erd).map_err(db)? {
        return Err(ApiErr::new(StatusCode::NOT_FOUND, "project not found"));
    }
    read(State(s), Auth(me), Path(id)).await
}

async fn remove(
    State(s): State<S>,
    Auth(me): Auth,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiErr> {
    if s.store.delete(&me, &id).map_err(db)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiErr::new(StatusCode::NOT_FOUND, "project not found"))
    }
}
