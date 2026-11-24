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
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tower_http::limit::RequestBodyLimitLayer;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<SqliteStore>,
    pub web_dir: Arc<PathBuf>,
    pub limiter: Arc<RateLimiter>,
    pub trust_proxy: bool,
    pub max_body_bytes: usize,
}

impl AppState {
    pub fn new(store: Arc<SqliteStore>, config: &crate::config::Config) -> Self {
        Self {
            store,
            web_dir: Arc::new(config.web_dir.clone()),
            limiter: Arc::new(RateLimiter::new(config.rate_per_minute)),
            trust_proxy: config.trust_proxy,
            max_body_bytes: config.max_body_bytes,
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .merge(auth::routes())
        .merge(mcp::routes())
        .merge(live::routes())
        .fallback(get(static_files::serve))
        .layer(RequestBodyLimitLayer::new(state.max_body_bytes))
        .layer(middleware::from_fn_with_state(state.clone(), rate_limit))
        .with_state(state)
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

/// The address the limit is counted against. Behind a trusted proxy it is the last `X-Forwarded-For` entry, the
/// one the proxy added; entries before it were written by the client and are ignored.
fn client_ip(req: &Request, trust_proxy: bool) -> IpAddr {
    if trust_proxy {
        let forwarded = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|s| s.trim().parse().ok());
        if let Some(ip) = forwarded {
            return ip;
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
        .check_at(client_ip(&req, state.trust_proxy), Instant::now())
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
