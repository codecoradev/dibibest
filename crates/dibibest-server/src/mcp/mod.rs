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
    with_cors(origin, serve(&state.auth, &ctx, req).await)
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
async fn serve(auth: &Auth, ctx: &Ctx<'_>, req: Request) -> Response {
    if req.method() != Method::POST {
        return json_response(
            405,
            json!({ "error": "method not allowed" }),
            &[("allow", "POST, OPTIONS")],
        );
    }
    let headers = req.headers().clone();
    let ua: String = header_text(&headers, "user-agent")
        .chars()
        .take(40)
        .collect();

    let Some(token) = bearer(&headers) else {
        tracing::info!("mcp: 401 no bearer ua={ua}");
        return unauthorized(ctx, false);
    };
    let unavailable = || {
        json_response(
            503,
            json!({ "error": "the identity provider cannot be reached right now" }),
            &[("retry-after", "30")],
        )
    };
    let Some(jwks_uri) = ctx.provider.discovery().await.ok().and_then(|d| d.jwks_uri) else {
        return unavailable();
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
            // visible in the server log; never the token
            tracing::warn!("mcp: token refused: {why}");
            return unauthorized(ctx, true);
        }
        Err(TokenError::Unavailable(_)) => return unavailable(),
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
        return too_large();
    }
    let Ok(bytes) = to_bytes(req.into_body(), MAX_BODY_BYTES + 1).await else {
        return too_large();
    };
    if bytes.len() > MAX_BODY_BYTES {
        return too_large();
    }

    let bad =
        |code: i64, message: &str| json_response(400, rpc::error(&Value::Null, code, message), &[]);
    let Ok(message) = serde_json::from_slice::<Value>(&bytes) else {
        return bad(-32700, "parse error");
    };
    if message.is_array() {
        return bad(-32600, "batching is not supported");
    }
    if !message.is_object() || message.get("jsonrpc") != Some(&json!("2.0")) {
        return bad(-32600, "invalid request");
    }
    // a notification (no id) or a client response (no method) gets no body
    let (Some(method), Some(id)) = (message.get("method"), message.get("id")) else {
        return StatusCode::ACCEPTED.into_response();
    };
    let Some(method) = method.as_str() else {
        return json_response(400, rpc::error(id, -32600, "invalid request"), &[]);
    };
    tracing::info!(
        "mcp: <- {method} ua={ua} pv={} mm={} accept={}",
        header_text(&headers, "mcp-protocol-version"),
        header_text(&headers, "mcp-method"),
        header_text(&headers, "accept")
            .chars()
            .take(60)
            .collect::<String>()
    );

    if rpc::is_modern(&message, method) {
        let (status, body) = rpc::modern(&headers, &message, method).await;
        tracing::info!("mcp: {method} (modern) -> {status}");
        return json_response(status, body, &[]);
    }
    let reply = rpc::legacy(&message, method).await;
    json_response(200, reply, &[])
}
