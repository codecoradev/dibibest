//! Sign-in and access tokens (epic #84, issue #93).
//!
//! - [`codec`]: base64url, the encoding of signed cookies and JWTs.
//! - [`session`]: HMAC-signed values (the login handshake and the session are signed cookies, nothing is stored),
//!   PKCE, the open-redirect guard and the same-origin check.
//! - [`jwt`]: verification of access tokens against the identity provider's JWKS, shared with the MCP endpoint.
//! - [`http`] and [`provider`]: the client for the identity provider and its discovery document.
//!
//! - [`flow`]: the routes, `/auth/me`, `/auth/login`, `/auth/callback` and `/auth/logout`.

use crate::app::AppState;
use crate::config::{Config, Oidc};
use axum::Router;
use std::time::Duration;

pub mod codec;
pub mod flow;
pub mod http;
pub mod jwt;
pub mod provider;
pub mod session;

/// What the sign-in routes and the MCP endpoint share: the settings, the HTTP client, the provider's discovery
/// and its keys. Built once, from the configuration.
pub struct Auth {
    /// `None` when sign-in is not configured.
    pub oidc: Option<Oidc>,
    /// The origin people reach the server on (`DIBIBEST_PUBLIC_URL`).
    pub public_origin: Option<String>,
    pub client: reqwest::Client,
    pub provider: Option<provider::Provider>,
    pub verifier: jwt::Verifier<http::ReqwestJwks>,
}

impl Auth {
    pub fn new(config: &Config) -> Self {
        let client = http::client(Duration::from_secs(10));
        Self {
            oidc: config.oidc.clone(),
            public_origin: config.public_url.clone(),
            provider: config
                .oidc
                .as_ref()
                .map(|o| provider::Provider::new(&o.issuer, client.clone())),
            verifier: jwt::Verifier::new(http::ReqwestJwks::new(client.clone())),
            client,
        }
    }
}

pub fn routes() -> Router<AppState> {
    flow::routes()
}
