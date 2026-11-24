//! The sign-in routes: `/auth/me`, `/auth/login`, `/auth/callback` and `/auth/logout`.
//!
//! Nothing is stored. The login handshake and the session are signed cookies (see `session`), in the format the
//! TypeScript sign-in used. The flow is the authorization code flow with PKCE: `login` sends the visitor to the
//! provider with a state and a code challenge; `callback` checks the state against the handshake cookie,
//! exchanges the code for an access token, reads the profile, and sets the session.

use super::http::{read_limited, MAX_BODY};
use super::session::{
    constant_time_eq, pkce_challenge, random_token, safe_return_to, same_origin, sign, verify,
    LOGIN_COOKIE, LOGIN_TTL_S, SESSION_COOKIE, SESSION_TTL_S,
};
use super::{provider::Discovery, Auth};
use crate::app::AppState;
use crate::config::{Oidc, TokenAuth};
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

const UNREACHABLE: &str = "CodeCora ID cannot be reached right now. The workspace still works without signing in; try again in a moment.";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/me", get(me))
        .route("/auth/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/auth/logout", post(logout))
}

#[derive(Serialize, Deserialize)]
struct Session {
    sub: String,
    email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    exp: i64,
}

#[derive(Serialize, Deserialize)]
struct Handshake {
    state: String,
    verifier: String,
    #[serde(rename = "returnTo")]
    return_to: String,
    exp: i64,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

// ── cookies ────────────────────────────────────────────────────────────────────────────────────────────

fn secure(auth: &Auth) -> bool {
    auth.public_origin
        .as_deref()
        .is_some_and(|o| o.starts_with("https://"))
}

/// `name=value; Path=..; Max-Age=..; HttpOnly; SameSite=Lax[; Secure]`. `Secure` follows the public address, so
/// it is on in production and off only on a plain-http localhost.
fn cookie(auth: &Auth, name: &str, value: &str, max_age: i64, path: &str) -> HeaderValue {
    let secure = if secure(auth) { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{name}={value}; Path={path}; Max-Age={max_age}; HttpOnly; SameSite=Lax{secure}"
    ))
    .expect("a cookie made of base64url and constants is a valid header")
}

fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|line| line.split(';'))
        .filter_map(|part| part.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

// ── responses ──────────────────────────────────────────────────────────────────────────────────────────

fn json_response(status: StatusCode, body: Value) -> Response {
    let mut res = (status, Json(body)).into_response();
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// A small page for a sign-in that did not work. It is locked down (no scripts, nothing embedded, no framing
/// concerns) because some of what it shows came from a link someone else wrote.
fn page(status: StatusCode, title: &str, body: &str, clear_login: Option<HeaderValue>) -> Response {
    let html = format!(
        "<!doctype html><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{t}</title><body style=\"font-family:system-ui,sans-serif;background:#0b0d10;color:#e6e8eb;display:grid;place-items:center;min-height:100vh;margin:0\"><main style=\"max-width:28rem;padding:1.5rem\"><h1 style=\"font-size:1.25rem\">{t}</h1><p>{b}</p><p><a style=\"color:#6ea8ff\" href=\"/app\">Back to the workspace</a></p></main></body>",
        t = escape(title),
        b = escape(body)
    );
    let mut res = (status, html).into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'",
        ),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    if let Some(c) = clear_login {
        h.append(header::SET_COOKIE, c);
    }
    res
}

fn not_configured() -> Response {
    json_response(
        StatusCode::NOT_FOUND,
        json!({ "error": "sign-in is not configured" }),
    )
}

// ── routes ─────────────────────────────────────────────────────────────────────────────────────────────

/// `GET /auth/me`: whether sign-in is on, and who is signed in. The web app reads this to gate import and export.
async fn me(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(oidc) = &state.auth.oidc else {
        return json_response(StatusCode::OK, json!({ "enabled": false, "user": null }));
    };
    let user = read_cookie(&headers, SESSION_COOKIE)
        .and_then(|raw| verify::<Session>(&oidc.session_secret, &raw, now()))
        .map(|s| {
            let mut u = json!({ "sub": s.sub, "email": s.email });
            if let Some(name) = s.name {
                u["name"] = json!(name);
            }
            u
        });
    json_response(StatusCode::OK, json!({ "enabled": true, "user": user }))
}

fn redirect_uri(auth: &Auth) -> String {
    format!(
        "{}/auth/callback",
        auth.public_origin.as_deref().unwrap_or_default()
    )
}

/// `GET /auth/login?return_to=/app`: 302 to the provider with PKCE S256 and a state.
async fn login(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let auth = &state.auth;
    let (Some(oidc), Some(provider)) = (&auth.oidc, &auth.provider) else {
        return not_configured();
    };
    let Ok(d) = provider.discovery().await else {
        return page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Sign-in unavailable",
            UNREACHABLE,
            None,
        );
    };
    let state_token = random_token(32);
    let verifier = random_token(48);
    let handshake = sign(
        &oidc.session_secret,
        &Handshake {
            state: state_token.clone(),
            verifier: verifier.clone(),
            return_to: safe_return_to(q.get("return_to").map(String::as_str)),
            exp: now() + LOGIN_TTL_S,
        },
    );
    let Ok(mut url) = Url::parse(&d.authorization_endpoint) else {
        return page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Sign-in unavailable",
            UNREACHABLE,
            None,
        );
    };
    url.query_pairs_mut()
        .clear()
        .append_pair("response_type", "code")
        .append_pair("client_id", &oidc.client_id)
        .append_pair("redirect_uri", &redirect_uri(auth))
        .append_pair("scope", "openid email profile")
        .append_pair("state", &state_token)
        .append_pair("code_challenge", &pkce_challenge(&verifier))
        .append_pair("code_challenge_method", "S256");
    let mut res = (StatusCode::FOUND, [(header::LOCATION, url.to_string())]).into_response();
    let h = res.headers_mut();
    h.append(
        header::SET_COOKIE,
        cookie(auth, LOGIN_COOKIE, &handshake, LOGIN_TTL_S, "/auth"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// JavaScript's `encodeURIComponent`: what RFC 6749 section 2.3.1 asks of the client id and secret, and what the
/// TypeScript sign-in did, so the provider sees the same bytes.
const URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

fn encode(s: &str) -> String {
    utf8_percent_encode(s, URI_COMPONENT).to_string()
}

/// What `callback` does once the state has matched. The error is the page to show.
async fn exchange(
    auth: &Auth,
    oidc: &Oidc,
    d: &Discovery,
    code: &str,
    verifier: &str,
) -> Result<Value, (StatusCode, &'static str, String)> {
    let unavailable = || {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Sign-in unavailable",
            UNREACHABLE.to_string(),
        )
    };
    let failed = |what: String| (StatusCode::BAD_GATEWAY, "Sign-in failed", what);

    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("redirect_uri", redirect_uri(auth)),
        ("code_verifier", verifier.to_string()),
    ];
    let mut request = auth
        .client
        .post(&d.token_endpoint)
        .header(header::ACCEPT, "application/json");
    match oidc.token_auth {
        TokenAuth::Basic => {
            let credentials = format!(
                "{}:{}",
                encode(&oidc.client_id),
                encode(&oidc.client_secret)
            );
            request = request.header(
                header::AUTHORIZATION,
                format!("Basic {}", STANDARD.encode(credentials)),
            );
        }
        TokenAuth::Post => {
            form.push(("client_id", oidc.client_id.clone()));
            form.push(("client_secret", oidc.client_secret.clone()));
        }
    }
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(&form)
        .finish();
    let response = request
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|_| unavailable())?;
    if !response.status().is_success() {
        return Err(failed(format!(
            "Token exchange failed ({}).",
            response.status().as_u16()
        )));
    }
    let token: Value = read_limited(response, MAX_BODY)
        .await
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let Some(access_token) = token
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    else {
        return Err(failed("No access token was returned.".into()));
    };

    let response = auth
        .client
        .get(&d.userinfo_endpoint)
        .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
        .header(header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|_| unavailable())?;
    if !response.status().is_success() {
        return Err(failed(format!(
            "Could not read your profile ({}).",
            response.status().as_u16()
        )));
    }
    Ok(read_limited(response, MAX_BODY)
        .await
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null))
}

/// `GET /auth/callback?code&state`: checks the state, exchanges the code, reads the profile, sets the session.
async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let auth = &state.auth;
    let (Some(oidc), Some(provider)) = (&auth.oidc, &auth.provider) else {
        return not_configured();
    };
    let clear_login = || Some(cookie(auth, LOGIN_COOKIE, "", 0, "/auth"));

    if let Some(err) = q.get("error") {
        // An OAuth error code is short and plain. Anything else came from a crafted link: do not repeat it.
        let shown = if !err.is_empty()
            && err.len() <= 64
            && err
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        {
            err.as_str()
        } else {
            "an error"
        };
        return page(
            StatusCode::BAD_REQUEST,
            "Sign-in was cancelled",
            &format!("The identity provider answered: {shown}."),
            clear_login(),
        );
    }
    let handshake = read_cookie(&headers, LOGIN_COOKIE)
        .and_then(|raw| verify::<Handshake>(&oidc.session_secret, &raw, now()));
    let code = q.get("code").filter(|c| !c.is_empty());
    let (Some(hs), Some(code)) = (handshake, code) else {
        return page(
            StatusCode::BAD_REQUEST,
            "Sign-in expired",
            "Please start signing in again.",
            clear_login(),
        );
    };
    let state_matches = q
        .get("state")
        .is_some_and(|s| constant_time_eq(s.as_bytes(), hs.state.as_bytes()));
    if !state_matches {
        return page(
            StatusCode::BAD_REQUEST,
            "Sign-in expired",
            "Please start signing in again.",
            clear_login(),
        );
    }

    let Ok(d) = provider.discovery().await else {
        return page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Sign-in unavailable",
            UNREACHABLE,
            clear_login(),
        );
    };
    let info = match exchange(auth, oidc, &d, code, &hs.verifier).await {
        Ok(info) => info,
        Err((status, title, what)) => {
            tracing::warn!("sign-in: {what}");
            return page(status, title, &what, clear_login());
        }
    };
    let text = |name: &str| {
        info.get(name)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let (Some(sub), Some(email)) = (text("sub"), text("email")) else {
        return page(
            StatusCode::BAD_GATEWAY,
            "Sign-in failed",
            "Your profile has no email address.",
            clear_login(),
        );
    };
    if info.get("email_verified") == Some(&Value::Bool(false)) {
        return page(
            StatusCode::FORBIDDEN,
            "Email not verified",
            "Verify your email with CodeCora ID, then sign in again.",
            clear_login(),
        );
    }
    let session = sign(
        &oidc.session_secret,
        &Session {
            sub: sub.to_string(),
            email: email.to_lowercase(),
            name: text("name").map(str::to_string),
            exp: now() + SESSION_TTL_S,
        },
    );
    // The address in the cookie is cleaned again: a handshake issued before an address-checking bug was fixed
    // is still correctly signed.
    let mut res = (
        StatusCode::FOUND,
        [(header::LOCATION, safe_return_to(Some(&hs.return_to)))],
    )
        .into_response();
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.append(
        header::SET_COOKIE,
        cookie(auth, SESSION_COOKIE, &session, SESSION_TTL_S, "/"),
    );
    h.append(
        header::SET_COOKIE,
        cookie(auth, LOGIN_COOKIE, "", 0, "/auth"),
    );
    res
}

/// `POST /auth/logout`: clears the session. Same origin only, so another site cannot sign a person out.
async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let auth = &state.auth;
    if !same_origin(&headers, auth.public_origin.as_deref().unwrap_or_default()) {
        return json_response(
            StatusCode::FORBIDDEN,
            json!({ "error": "cross-site request refused" }),
        );
    }
    let mut res = (StatusCode::SEE_OTHER, [(header::LOCATION, "/")]).into_response();
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.append(header::SET_COOKIE, cookie(auth, SESSION_COOKIE, "", 0, "/"));
    res
}

#[cfg(test)]
mod tests {
    use super::escape;

    #[test]
    fn markup_and_quotes_are_escaped() {
        assert_eq!(
            escape("<script>alert('x') & \"y\"</script>"),
            "&lt;script&gt;alert(&#39;x&#39;) &amp; &quot;y&quot;&lt;/script&gt;"
        );
        assert_eq!(escape("plain text"), "plain text");
    }
}
