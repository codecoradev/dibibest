//! Signed values, PKCE and the guards of the sign-in flow.
//!
//! Nothing is stored on the server. The login handshake and the session are `b64url(json).b64url(HMAC-SHA256)`
//! values in cookies, so a restart or a second process keeps everyone signed in. The format is the one the
//! TypeScript sign-in used, so cookies it issued stay valid after the cutover.

use super::codec::{b64url, b64url_decode};
use axum::http::HeaderMap;
use ring::rand::{SecureRandom, SystemRandom};
use ring::{digest, hmac};
use serde::de::DeserializeOwned;
use serde::Serialize;

pub const SESSION_COOKIE: &str = "dibibest_session";
pub const LOGIN_COOKIE: &str = "dibibest_login";
pub const SESSION_TTL_S: i64 = 7 * 24 * 3600;
pub const LOGIN_TTL_S: i64 = 10 * 60;

/// Where to go after signing in when `return_to` is missing or not trusted.
const DEFAULT_RETURN_TO: &str = "/app";

/// `b64url(json).b64url(HMAC-SHA256)`. The payload must serialise to an object with a numeric `exp` (unix
/// seconds); [`verify`] refuses a value without one.
pub fn sign<T: Serialize>(secret: &str, payload: &T) -> String {
    let body = b64url(&serde_json::to_vec(payload).expect("a payload serialises"));
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    format!(
        "{body}.{}",
        b64url(hmac::sign(&key, body.as_bytes()).as_ref())
    )
}

/// The payload, when the signature is valid (compared in constant time) and `exp` is still in the future.
pub fn verify<T: DeserializeOwned>(secret: &str, token: &str, now: i64) -> Option<T> {
    let mut parts = token.split('.');
    let (body, mac) = (parts.next()?, parts.next()?);
    if body.is_empty() || mac.is_empty() || parts.next().is_some() {
        return None;
    }
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    hmac::verify(&key, body.as_bytes(), &b64url_decode(mac)?).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&b64url_decode(body)?).ok()?;
    let exp = value.get("exp")?.as_f64()?;
    if exp <= now as f64 {
        return None;
    }
    serde_json::from_value(value).ok()
}

/// `bytes` random bytes from the system, as base64url. A failing system RNG cannot be recovered from, so it
/// panics rather than hand out a guessable token.
pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    SystemRandom::new()
        .fill(&mut buf)
        .expect("the system random number generator works");
    b64url(&buf)
}

/// The PKCE S256 challenge of `verifier` (RFC 7636).
pub fn pkce_challenge(verifier: &str) -> String {
    b64url(digest::digest(&digest::SHA256, verifier.as_bytes()).as_ref())
}

/// Only same-site relative paths (the open-redirect guard). Control characters and backslashes are refused
/// outright: browsers strip tab, CR and LF and read a backslash as a slash when they parse a `Location`
/// (`/\t/evil` becomes `//evil`). What is left is resolved against a placeholder origin, which also normalises it
/// (`/a/../app` is `/app`), and has to stay on that origin.
pub fn safe_return_to(value: Option<&str>) -> String {
    let fallback = || DEFAULT_RETURN_TO.to_string();
    let Some(v) = value else { return fallback() };
    if v.is_empty()
        || v.chars()
            .any(|c| c <= '\u{1f}' || c == '\u{7f}' || c == '\\')
    {
        return fallback();
    }
    if !v.starts_with('/') || v.starts_with("//") {
        return fallback();
    }
    let base = url::Url::parse("https://return-to.invalid").expect("a constant URL parses");
    match base.join(v) {
        Ok(u) if u.origin() == base.origin() => {
            let mut out = u.path().to_string();
            if let Some(q) = u.query() {
                out.push('?');
                out.push_str(q);
            }
            if let Some(f) = u.fragment() {
                out.push('#');
                out.push_str(f);
            }
            out
        }
        _ => fallback(),
    }
}

/// The CSRF guard of state-changing requests: the `Origin` must be this server's origin; without one, fetch
/// metadata must say `same-origin` or `same-site`. With neither there is nothing to go on, and the answer is no.
pub fn same_origin(headers: &HeaderMap, origin: &str) -> bool {
    if let Some(o) = headers.get("origin") {
        return o.to_str().ok() == Some(origin);
    }
    matches!(
        headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()),
        Some("same-origin" | "same-site")
    )
}
