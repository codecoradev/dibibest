//! dibibest-api: Gatehouse OIDC relying party + saved-projects storage.
//! The web SPA stays static; this server owns auth and persistence.

pub mod app;
pub mod config;
pub mod session;
pub mod store;
