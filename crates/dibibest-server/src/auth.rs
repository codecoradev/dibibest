//! Routes of the `auth` module. Empty until its stage lands; the router already mounts it, so adding routes does
//! not touch the composition in `app.rs`.

use crate::app::AppState;
use axum::Router;

pub fn routes() -> Router<AppState> {
    Router::new()
}
