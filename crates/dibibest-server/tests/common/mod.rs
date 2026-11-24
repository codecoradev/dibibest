#![allow(dead_code)]
use axum::body::{to_bytes, Body};
use axum::http::{Request, Response, StatusCode};
use axum::Router;
use dibibest_server::app::{router, AppState};
use dibibest_server::config::Config;
use dibibest_store::SqliteStore;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

pub struct Fixture {
    pub dir: TempDir,
    pub web: std::path::PathBuf,
    pub config: Config,
}

/// A data directory and a small built web app in a temp directory.
pub fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let web = dir.path().join("web");
    std::fs::create_dir_all(web.join("_app/immutable/chunks")).unwrap();
    std::fs::write(web.join("index.html"), "<html>INDEX</html>").unwrap();
    std::fs::write(web.join("_app/immutable/chunks/a.js"), "console.log(1)").unwrap();
    std::fs::write(web.join("core.wasm"), b"\0asm\x01\0\0\0").unwrap();
    std::fs::write(web.join("robots.txt"), "User-agent: *").unwrap();
    std::fs::write(dir.path().join("secret.txt"), "TOP SECRET").unwrap();
    let config = Config {
        data_dir: dir.path().join("data"),
        web_dir: web.clone(),
        ..Config::default()
    };
    Fixture { dir, web, config }
}

pub fn app(fx: &Fixture) -> Router {
    std::fs::create_dir_all(&fx.config.data_dir).unwrap();
    let store = SqliteStore::open(fx.config.data_dir.join("dibibest.db")).unwrap();
    router(AppState::new(Arc::new(store), &fx.config))
}

pub async fn get(app: &Router, uri: &str) -> Response<Body> {
    send(
        app,
        Request::builder().uri(uri).body(Body::empty()).unwrap(),
    )
    .await
}

pub async fn send(app: &Router, req: Request<Body>) -> Response<Body> {
    app.clone().oneshot(req).await.unwrap()
}

pub async fn text(res: Response<Body>) -> (StatusCode, String) {
    let status = res.status();
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

pub fn header(res: &Response<Body>, name: &str) -> String {
    res.headers()
        .get(name)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default()
}

pub fn exists(p: &Path) -> bool {
    p.exists()
}
