//! Sign-in and access tokens (epic #84, issue #93).
//!
//! - [`codec`]: base64url, the encoding of signed cookies and JWTs.
//! - [`session`]: HMAC-signed values (the login handshake and the session are signed cookies, nothing is stored),
//!   PKCE, the open-redirect guard and the same-origin check.
//! - [`jwt`]: verification of access tokens against the identity provider's JWKS, shared with the MCP endpoint.
//!
//! The routes are mounted by `app.rs` from [`routes`]; they arrive with the next change.

use crate::app::AppState;
use axum::Router;

pub mod codec;
pub mod jwt;
pub mod session;

pub fn routes() -> Router<AppState> {
    Router::new()
}
