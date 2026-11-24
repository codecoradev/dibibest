//! The remote MCP endpoint (epic #84, issue #94): Streamable HTTP, stateless, JSON answers only (no SSE, no
//! sessions). Every request needs an OAuth access token from the identity provider for this resource; there are no
//! API keys.
//!
//! - `POST /mcp`: JSON-RPC (see [`rpc`] for the two protocol eras), with the tools in [`tools`].
//! - `GET /.well-known/oauth-protected-resource[/mcp]`: RFC 9728 metadata pointing at the provider ([`meta`]).
//! - `GET /.well-known/oauth-authorization-server[/mcp]` and `/.well-known/openid-configuration[/mcp]`: the
//!   provider's own metadata, unchanged, because tokens carry the provider's `iss`.
//!
//! Clients find the provider from the `resource_metadata` of a 401, register by Client ID Metadata Document, and
//! ask for `resource=<public origin>/mcp`. The resource URL comes from `DIBIBEST_PUBLIC_URL`, never from the
//! `Host` header, which a client can write.
//!
//! The behaviour is pinned by the 55 fixtures in `apps/web/server/fixtures/mcp`, which the TypeScript server
//! passed first.

mod meta;
mod rpc;
mod tools;

use crate::app::AppState;
use crate::auth::jwt::{TokenError, VerifyOptions};
use crate::auth::Auth;
use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::Router;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

/// The largest request body. The server-wide limit is larger; this one is the MCP's own.
pub const MAX_BODY_BYTES: usize = 512 * 1024;

const CORS_ALLOW_HEADERS: &str =
    "authorization, content-type, accept, mcp-protocol-version, mcp-session-id, mcp-method, mcp-name, last-event-id";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/mcp", any(mcp))
        .route(
            "/.well-known/oauth-protected-resource",
            get(meta::protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(meta::protected_resource),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(meta::authorization_server),
        )
        .route(
            "/.well-known/oauth-authorization-server/mcp",
            get(meta::authorization_server),
        )
        .route(
            "/.well-known/openid-configuration",
            get(meta::authorization_server),
        )
        .route(
            "/.well-known/openid-configuration/mcp",
            get(meta::authorization_server),
        )
}

/// What the endpoint needs to be on: a provider and the public address. Without both it answers 404.
pub(crate) struct Ctx<'a> {
    pub issuer: &'a str,
    pub origin: &'a str,
    pub provider: &'a crate::auth::provider::Provider,
}

impl<'a> Ctx<'a> {
    pub fn new(auth: &'a Auth) -> Option<Self> {
        Some(Self {
            issuer: auth.issuer.as_deref()?,
            origin: auth.public_origin.as_deref()?,
            provider: auth.provider.as_ref()?,
        })
    }

    pub fn resource(&self) -> String {
        format!("{}/mcp", self.origin)
    }

    fn metadata_url(&self) -> String {
        format!("{}/.well-known/oauth-protected-resource/mcp", self.origin)
    }
}

pub(crate) fn json_response(
    status: u16,
    body: Value,
    extra: &[(&'static str, &'static str)],
) -> Response {
    let mut res = (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        body.to_string(),
    )
        .into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    for (k, v) in extra {
        h.insert(*k, HeaderValue::from_static(v));
    }
    res
}

pub(crate) fn not_configured() -> Response {
    json_response(404, json!({ "error": "remote MCP is not configured" }), &[])
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// CORS for MCP clients that call from a browser origin (claude.ai sends `Origin: https://claude.ai`). Every
/// request needs a bearer token and no cookie is read, so a page on another origin cannot act for a person. That is
/// why any origin is echoed instead of refused: a 403 here broke the Claude connector (#68).
fn with_cors(origin: Option<HeaderValue>, mut res: Response) -> Response {
    let Some(origin) = origin else { return res };
    let h = res.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    h.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("www-authenticate, mcp-session-id, mcp-protocol-version"),
    );
    h.append(header::VARY, HeaderValue::from_static("origin"));
    res
}

/// `/mcp`: the CORS preflight, then the transport.
async fn mcp(State(state): State<AppState>, req: Request) -> Response {
    let Some(ctx) = Ctx::new(&state.auth) else {
        return not_configured();
    };
    let origin = req.headers().get(header::ORIGIN).cloned();
    if req.method() == Method::OPTIONS {
        let mut res = StatusCode::NO_CONTENT.into_response();
        let h = res.headers_mut();
        h.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("POST, GET, DELETE, OPTIONS"),
        );
        h.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(CORS_ALLOW_HEADERS),
        );
        h.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static("86400"),
        );
        return with_cors(origin, res);
    }
    let mut call = Call::default();
    let res = serve(&state.auth, &ctx, req, &mut call).await;
    call.log(res.status());
    with_cors(origin, res)
}

/// What is known about one call to `/mcp`, for the single log line it leaves. Never the token or any of the body:
/// only the method name, a few headers (quoted and cut, see `clip`) and why the call was refused, if it was.
#[derive(Default)]
struct Call {
    method: Option<String>,
    ua: Option<String>,
    protocol_version: Option<String>,
    reason: Option<String>,
}

impl Call {
    fn log(&self, status: StatusCode) {
        let shown = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".to_string());
        tracing::info!(
            "mcp: status={} method={} ua={} pv={} reason={}",
            status.as_u16(),
            shown(&self.method),
            shown(&self.ua),
            shown(&self.protocol_version),
            shown(&self.reason)
        );
    }

    /// Records why a call was refused and passes the response on.
    fn refused(&mut self, why: &str, res: Response) -> Response {
        self.reason = Some(clip(why, 120));
        res
    }
}

fn unauthorized(ctx: &Ctx<'_>, invalid_token: bool) -> Response {
    let challenge = if invalid_token {
        format!(
            "Bearer resource_metadata=\"{}\", error=\"invalid_token\"",
            ctx.metadata_url()
        )
    } else {
        format!("Bearer resource_metadata=\"{}\"", ctx.metadata_url())
    };
    let mut res = json_response(
        401,
        json!({
            "error": if invalid_token { "invalid_token" } else { "unauthorized" },
            "error_description": "an OAuth access token for this resource is required"
        }),
        &[],
    );
    if let Ok(v) = HeaderValue::from_str(&challenge) {
        res.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    res
}

/// The token of an `Authorization: Bearer <token>` header; the token is one run of characters of its alphabet.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?.trim();
    let (scheme, rest) = value.split_once(char::is_whitespace)?;
    let token = rest.trim_start();
    let alphabet = |c: char| c.is_ascii_alphanumeric() || "._~+/=-".contains(c);
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() && token.chars().all(alphabet))
        .then_some(token)
}

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
}

/// POST /mcp (405 for the other methods: no server-initiated stream, no sessions).
async fn serve(auth: &Auth, ctx: &Ctx<'_>, req: Request, call: &mut Call) -> Response {
    call.ua = Some(clip(header_text(req.headers(), "user-agent"), 40));
    call.protocol_version = Some(clip(header_text(req.headers(), "mcp-protocol-version"), 40));
    if req.method() != Method::POST {
        let res = json_response(
            405,
            json!({ "error": "method not allowed" }),
            &[("allow", "POST, OPTIONS")],
        );
        return call.refused("method not allowed", res);
    }
    let headers = req.headers().clone();

    let Some(token) = bearer(&headers) else {
        return call.refused("no bearer token", unauthorized(ctx, false));
    };
    let unavailable = || {
        json_response(
            503,
            json!({ "error": "the identity provider cannot be reached right now" }),
            &[("retry-after", "30")],
        )
    };
    let Some(jwks_uri) = ctx.provider.discovery().await.ok().and_then(|d| d.jwks_uri) else {
        return call.refused("the identity provider cannot be reached", unavailable());
    };
    let resource = ctx.resource();
    let opts = VerifyOptions {
        issuer: ctx.issuer,
        audience: &resource,
        jwks_uri: &jwks_uri,
        now: now(),
    };
    match auth.verifier.verify(token, &opts).await {
        Ok(_) => {}
        Err(TokenError::Invalid(why)) => {
            // the reason is logged, never the token
            return call.refused(&format!("token refused: {why}"), unauthorized(ctx, true));
        }
        Err(TokenError::Unavailable(_)) => {
            return call.refused("the signing keys cannot be fetched", unavailable());
        }
    }

    let too_large = || {
        json_response(
            413,
            rpc::error(&Value::Null, -32600, "request too large"),
            &[],
        )
    };
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > MAX_BODY_BYTES) {
        return call.refused("request too large", too_large());
    }
    let Ok(bytes) = to_bytes(req.into_body(), MAX_BODY_BYTES + 1).await else {
        return call.refused("request too large", too_large());
    };
    if bytes.len() > MAX_BODY_BYTES {
        return call.refused("request too large", too_large());
    }

    let bad = |call: &mut Call, code: i64, message: &str| {
        let res = json_response(400, rpc::error(&Value::Null, code, message), &[]);
        call.refused(message, res)
    };
    let Ok(message) = serde_json::from_slice::<Value>(&bytes) else {
        return bad(call, -32700, "parse error");
    };
    if message.is_array() {
        return bad(call, -32600, "batching is not supported");
    }
    if !message.is_object() || message.get("jsonrpc") != Some(&json!("2.0")) {
        return bad(call, -32600, "invalid request");
    }
    // a notification (no id) or a client response (no method) gets no body
    let (Some(method), Some(id)) = (message.get("method"), message.get("id")) else {
        return StatusCode::ACCEPTED.into_response();
    };
    let Some(method) = method.as_str() else {
        let res = json_response(400, rpc::error(id, -32600, "invalid request"), &[]);
        return call.refused("invalid request", res);
    };
    call.method = Some(clip(method, 60));

    if rpc::is_modern(&message, method) {
        let (status, body) = rpc::modern(&headers, &message, method).await;
        if status >= 400 {
            let why = body["error"]["message"].as_str().unwrap_or("refused");
            call.reason = Some(clip(why, 120));
        }
        return json_response(status, body, &[]);
    }
    let reply = rpc::legacy(&message, method).await;
    json_response(200, reply, &[])
}

/// A value from the request, cut short and quoted with its control characters escaped, so that nothing a client
/// sends can forge or split a log line.
fn clip(text: &str, max: usize) -> String {
    format!("{:?}", text.chars().take(max).collect::<String>())
}

#[cfg(test)]
mod clip_tests {
    use super::clip;

    #[test]
    fn a_logged_value_is_cut_short_and_cannot_break_the_line() {
        assert_eq!(clip("abcdef", 3), "\"abc\"");
        let forged = clip("x\nmcp: 200 admin logged in\r\u{1b}[31m", 100);
        assert!(!forged.contains('\n') && !forged.contains('\r') && !forged.contains('\u{1b}'));
        assert!(forged.starts_with('"') && forged.ends_with('"'));
    }
}
