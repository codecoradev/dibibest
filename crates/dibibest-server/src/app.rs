//! The router: probes, the modules, the web app, and the limits in front of all of it.

use crate::limit::RateLimiter;
use crate::{auth, live, mcp, static_files};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use dibibest_store::{SqliteStore, StoreError};
use serde_json::json;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tower_http::limit::RequestBodyLimitLayer;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<SqliteStore>,
    pub web_dir: Arc<PathBuf>,
    pub limiter: Arc<RateLimiter>,
    pub trusted_proxies: usize,
    pub max_body_bytes: usize,
    pub header_timeout: std::time::Duration,
    pub request_timeout: std::time::Duration,
    pub max_connections: usize,
    pub auth: Arc<crate::auth::Auth>,
}

impl AppState {
    pub fn new(store: Arc<SqliteStore>, config: &crate::config::Config) -> Self {
        Self {
            store,
            web_dir: Arc::new(config.web_dir.clone()),
            limiter: Arc::new(RateLimiter::new(config.rate_per_minute)),
            trusted_proxies: config.trusted_proxies,
            max_body_bytes: config.max_body_bytes,
            header_timeout: config.header_timeout,
            request_timeout: config.request_timeout,
            max_connections: config.max_connections,
            auth: Arc::new(crate::auth::Auth::new(config)),
        }
    }
}

pub fn router(state: AppState) -> Router {
    router_with(state, Router::new())
}

/// The router with extra routes inside the same limits. It is how a test adds a route that misbehaves (a slow
/// one) without the server having one; nothing in production passes anything but [`router`].
pub fn router_with(state: AppState, extra: Router<AppState>) -> Router {
    Router::new()
        .merge(extra)
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .merge(auth::routes())
        .merge(mcp::routes())
        .merge(live::routes())
        .fallback(get(static_files::serve))
        .layer(RequestBodyLimitLayer::new(state.max_body_bytes))
        .layer(middleware::from_fn_with_state(state.clone(), rate_limit))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            request_timeout,
        ))
        .with_state(state)
}

/// Gives a request `request_timeout` from the moment its headers are in to the moment its response is ready,
/// and answers 408 if it is not.
///
/// The one timer covers the body as well as the handler. A handler that reads the body is waiting inside the
/// timer while a slow client dribbles it in, and a handler that is simply slow is waiting too, so neither can
/// hold the connection. The handler's future is dropped when the time is up, which also drops the body it was
/// reading.
///
/// 408 and not 504: 408 is the server saying it gave up waiting for the request, which is what happened in both
/// cases and is the client's to retry, while 504 says a gateway got no answer from an upstream, and this server
/// is the origin. The answer carries `Connection: close`, because a body that was only half read cannot be told
/// from the start of the next request, so the connection ends after it.
///
/// Only the time until the response starts is bounded. A response that is streamed (an event stream, a WebSocket
/// after its 101) is not cut off by it.
async fn request_timeout(State(state): State<AppState>, req: Request, next: Next) -> Response {
    match tokio::time::timeout(state.request_timeout, next.run(req)).await {
        Ok(res) => res,
        Err(_) => {
            let mut res = static_files::json_error(StatusCode::REQUEST_TIMEOUT, "request_timeout");
            res.headers_mut()
                .insert(header::CONNECTION, HeaderValue::from_static("close"));
            res
        }
    }
}

/// Liveness: the process answers.
async fn healthz() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

/// Readiness: the store is open in WAL and can take writes.
async fn readyz(State(state): State<AppState>) -> Response {
    let store = state.store.clone();
    let probe = tokio::task::spawn_blocking(move || {
        store.pragmas().map(|p| (p.journal_mode, store.read_only()))
    })
    .await;
    let (status, body) = match probe {
        Ok(Ok((mode, read_only))) => {
            let wal = mode.eq_ignore_ascii_case("wal");
            let ok = wal && !read_only;
            let word = if read_only {
                "read_only"
            } else if wal {
                "ok"
            } else {
                "not_ready"
            };
            let code = if ok {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            (
                code,
                json!({ "status": word, "wal": wal, "read_only": read_only }),
            )
        }
        Ok(Err(StoreError::Busy)) => (StatusCode::SERVICE_UNAVAILABLE, json!({ "status": "busy" })),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            json!({ "status": "unavailable" }),
        ),
    };
    (status, Json(body)).into_response()
}

/// The key of every request whose forwarded header cannot be trusted at all. It is not a real client address, so
/// those requests share one bucket and are limited together, apart from the buckets of real clients and of
/// requests that reach the server directly.
const MALFORMED: IpAddr = IpAddr::V6(Ipv6Addr::UNSPECIFIED);

/// The address the limit is counted against.
///
/// With `trusted_proxies = N`, it is the Nth entry from the end of `X-Forwarded-For`, counting every header line
/// (a proxy may add a line of its own instead of extending the client's). The proxies wrote the last N entries;
/// everything before them was written by the client and is ignored.
///
/// - No header, or fewer entries than N: the request did not come through the proxies, so the connection's own
///   address counts (a `curl` from the host works).
/// - A line that cannot be read as text, or the chosen entry is not an address: the header is malformed and the
///   request gets the shared [`MALFORMED`] key. It is never charged to the proxy's own address, where it could use
///   up the budget of requests that reach the server directly, and it never gets a bucket of its own to rotate.
fn client_ip(req: &Request, trusted_proxies: usize) -> IpAddr {
    if trusted_proxies > 0 {
        let mut entries: Vec<&str> = Vec::new();
        for line in req.headers().get_all("x-forwarded-for") {
            let Ok(line) = line.to_str() else {
                return MALFORMED;
            };
            entries.extend(line.split(',').map(str::trim));
        }
        if entries.len() >= trusted_proxies {
            return entries[entries.len() - trusted_proxies]
                .parse()
                .unwrap_or(MALFORMED);
        }
    }
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(IpAddr::V4(Ipv4Addr::LOCALHOST), |c| c.0.ip())
}

async fn rate_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    // Probes come from the host and cost nothing; refusing them would take the service out of rotation.
    if matches!(req.uri().path(), "/healthz" | "/readyz") {
        return next.run(req).await;
    }
    match state
        .limiter
        .check_at(client_ip(&req, state.trusted_proxies), Instant::now())
    {
        Ok(()) => next.run(req).await,
        Err(wait) => {
            let mut res = static_files::json_error(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
            let secs = wait.as_secs_f64().ceil().max(1.0) as u64;
            if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
                res.headers_mut().insert(header::RETRY_AFTER, v);
            }
            res
        }
    }
}
