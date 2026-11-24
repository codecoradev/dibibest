mod common;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use common::*;
use dibibest_server::app::{router, AppState};
use dibibest_store::{OpInput, SqliteStore, Store, StoreError};
use std::sync::Arc;

#[tokio::test]
async fn healthz_says_the_process_is_up() {
    let fx = fixture();
    let (status, body) = text(get(&app(&fx), "/healthz").await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["status"],
        "ok"
    );
}

#[tokio::test]
async fn readyz_says_the_store_is_open_in_wal_and_writable() {
    let fx = fixture();
    let (status, body) = text(get(&app(&fx), "/readyz").await).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["status"], "ok");
    assert_eq!(v["wal"], true);
    assert_eq!(v["read_only"], false);
}

#[tokio::test]
async fn readyz_is_503_when_the_disk_is_full() {
    let fx = fixture();
    std::fs::create_dir_all(&fx.config.data_dir).unwrap();
    let opts = dibibest_store::Options {
        max_page_count: Some(64),
        ..Default::default()
    };
    let store = SqliteStore::open_with(fx.config.data_dir.join("dibibest.db"), opts).unwrap();
    store.create_project("a", 1).unwrap();
    let big = "x".repeat(50_000);
    for i in 0.. {
        let op = OpInput {
            key: format!("k{i}"),
            actor: "u".into(),
            payload: big.clone(),
            schema_version: 1,
        };
        match store.append("a", op) {
            Ok(_) => {}
            Err(StoreError::Full) => break,
            Err(e) => panic!("{e:?}"),
        }
        assert!(i < 1000);
    }
    let app = router(AppState::new(Arc::new(store), &fx.config));
    let (status, body) = text(get(&app, "/readyz").await).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["read_only"], true);
    // Liveness is separate: the process is still up.
    assert_eq!(get(&app, "/healthz").await.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_web_app_is_served_at_the_root_and_never_cached_hard() {
    let fx = fixture();
    let res = get(&app(&fx), "/").await;
    assert_eq!(header(&res, "cache-control"), "no-cache");
    assert!(header(&res, "content-type").starts_with("text/html"));
    let (status, body) = text(res).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("INDEX"));
}

#[tokio::test]
async fn a_client_route_falls_back_to_the_app() {
    let fx = fixture();
    let (status, body) = text(get(&app(&fx), "/p/some-project-id").await).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("INDEX"));
}

#[tokio::test]
async fn hashed_assets_are_cached_for_a_year_and_typed() {
    let fx = fixture();
    let res = get(&app(&fx), "/_app/immutable/chunks/a.js").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        header(&res, "cache-control"),
        "public, max-age=31536000, immutable"
    );
    assert!(header(&res, "content-type").contains("javascript"));
}

#[tokio::test]
async fn other_files_are_revalidated_and_wasm_has_its_type() {
    let fx = fixture();
    let app = app(&fx);
    let wasm = get(&app, "/core.wasm").await;
    assert_eq!(wasm.status(), StatusCode::OK);
    assert_eq!(header(&wasm, "content-type"), "application/wasm");
    assert_eq!(header(&wasm, "cache-control"), "no-cache");
    assert_eq!(get(&app, "/robots.txt").await.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_missing_file_with_an_extension_is_404_not_the_app() {
    let fx = fixture();
    let (status, body) = text(get(&app(&fx), "/missing.js").await).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!body.contains("INDEX"));
}

#[tokio::test]
async fn reserved_prefixes_answer_404_json_not_the_app() {
    let fx = fixture();
    let app = app(&fx);
    for path in [
        "/v1/nothing",
        "/auth/nothing",
        "/mcp",
        "/.well-known/nothing",
    ] {
        let res = get(&app, path).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path}");
        assert!(header(&res, "content-type").contains("json"), "{path}");
        let (_, body) = text(res).await;
        assert!(!body.contains("INDEX"), "{path}");
    }
}

#[tokio::test]
async fn paths_cannot_climb_out_of_the_web_directory() {
    let fx = fixture();
    let app = app(&fx);
    for path in [
        "/../secret.txt",
        "/..%2fsecret.txt",
        "/%2e%2e/secret.txt",
        "/_app/../../secret.txt",
        "/..%5csecret.txt",
        "/a%00b",
    ] {
        let (status, body) = text(get(&app, path).await).await;
        assert!(!body.contains("TOP SECRET"), "{path} leaked the file");
        assert!(
            status == StatusCode::NOT_FOUND || status == StatusCode::BAD_REQUEST,
            "{path}: {status}"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_inside_the_web_directory_cannot_point_outside_it() {
    let fx = fixture();
    std::os::unix::fs::symlink(fx.dir.path().join("secret.txt"), fx.web.join("leak.txt")).unwrap();
    let (status, body) = text(get(&app(&fx), "/leak.txt").await).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!body.contains("TOP SECRET"));
}

#[tokio::test]
async fn without_a_built_web_app_the_server_still_answers_and_says_so() {
    let mut fx = fixture();
    fx.config.web_dir = fx.dir.path().join("does-not-exist");
    let app = app(&fx);
    let (status, body) = text(get(&app, "/").await).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("not built"), "{body}");
    assert_eq!(get(&app, "/healthz").await.status(), StatusCode::OK);
}

#[tokio::test]
async fn only_get_and_head_read_the_app() {
    let fx = fixture();
    let app = app(&fx);
    let post = send(
        &app,
        Request::builder()
            .method(Method::POST)
            .uri("/")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(post.status(), StatusCode::METHOD_NOT_ALLOWED);
    let head = send(
        &app,
        Request::builder()
            .method(Method::HEAD)
            .uri("/")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(head.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_body_over_the_limit_is_413() {
    let mut fx = fixture();
    fx.config.max_body_bytes = 1024;
    let app = app(&fx);
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/anything")
        .header("content-length", "2048")
        .body(Body::from(vec![b'x'; 2048]))
        .unwrap();
    assert_eq!(
        send(&app, req).await.status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
}
