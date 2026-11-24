//! The built web app, served from a directory.
//!
//! It is a directory and not bytes inside the binary on purpose: embedding would make `cargo build` wait for a
//! web build (and the WASM toolchain), and CI builds the Rust workspace without one. The Docker image copies the
//! build next to the binary, so it is still one container.
//!
//! - A path that is a file is served; a path without an extension that is not a file gets `index.html` (the app's
//!   own routes, such as `/p/<id>`); a missing file with an extension is a plain 404.
//! - The prefixes the server itself owns answer a JSON 404, never the app.
//! - Hashed build output (`/_app/immutable/`) is cached for a year, everything else is revalidated.
//! - A path cannot leave the directory: `..` segments, backslashes, NUL, and symlinks that resolve outside it
//!   are all refused.

use crate::app::AppState;
use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use std::path::{Path, PathBuf};

const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const REVALIDATE: &str = "no-cache";

/// Paths the server answers itself (now or in a later stage); the app never stands in for them.
fn is_reserved(path: &str) -> bool {
    ["/v1", "/auth", "/mcp", "/.well-known"].iter().any(|p| {
        path == *p
            || path
                .strip_prefix(p)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

pub async fn serve(State(state): State<AppState>, uri: Uri) -> Response {
    let path = uri.path();
    if is_reserved(path) {
        return json_error(StatusCode::NOT_FOUND, "not_found");
    }
    let Some(rel) = safe_relative(path) else {
        return json_error(StatusCode::BAD_REQUEST, "bad_path");
    };
    let Ok(root) = tokio::fs::canonicalize(state.web_dir.as_ref()).await else {
        return not_built();
    };
    if !rel.as_os_str().is_empty() {
        if let Ok(found) = tokio::fs::canonicalize(root.join(&rel)).await {
            if found.starts_with(&root) && found.is_file() {
                return file(&found, &rel).await;
            }
        }
        if rel.extension().is_some() {
            return json_error(StatusCode::NOT_FOUND, "not_found");
        }
    }
    index(&root).await
}

/// The request path as a relative path, or `None` if it is not safe to look up.
fn safe_relative(path: &str) -> Option<PathBuf> {
    let decoded = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .ok()?;
    if decoded.contains('\0') || decoded.contains('\\') {
        return None;
    }
    let mut out = PathBuf::new();
    for segment in decoded.split('/') {
        match segment {
            "" | "." => {}
            ".." => return None,
            s => out.push(s),
        }
    }
    Some(out)
}

async fn index(root: &Path) -> Response {
    match tokio::fs::read(root.join("index.html")).await {
        Ok(bytes) => with_headers(bytes, "text/html; charset=utf-8", REVALIDATE),
        Err(_) => not_built(),
    }
}

async fn file(found: &Path, rel: &Path) -> Response {
    match tokio::fs::read(found).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(found).first_or_octet_stream();
            let cache = if rel.starts_with("_app/immutable") {
                IMMUTABLE
            } else {
                REVALIDATE
            };
            with_headers(bytes, mime.as_ref(), cache)
        }
        Err(_) => json_error(StatusCode::NOT_FOUND, "not_found"),
    }
}

fn with_headers(bytes: Vec<u8>, content_type: &str, cache: &'static str) -> Response {
    let mut res = bytes.into_response();
    let h = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(content_type) {
        h.insert(header::CONTENT_TYPE, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    res
}

fn not_built() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "the web app is not built (no index.html in the web directory)",
    )
        .into_response()
}

pub fn json_error(status: StatusCode, code: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        format!("{{\"error\":\"{code}\"}}"),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::safe_relative;
    use std::path::PathBuf;

    #[test]
    fn dot_dot_in_any_spelling_is_refused_before_the_file_system_is_asked() {
        for bad in [
            "/..",
            "/../x",
            "/a/../x",
            "/a/..",
            "/%2e%2e/x",
            "/%2E%2E/x",
            "/a%2f..%2fx",
            "/a\\x",
            "/a%5cx",
            "/a%00",
            "/%ff", // not UTF-8
        ] {
            assert_eq!(safe_relative(bad), None, "{bad}");
        }
    }

    #[test]
    fn ordinary_paths_become_relative_paths() {
        assert_eq!(safe_relative("/"), Some(PathBuf::new()));
        assert_eq!(safe_relative("//a///b/./c"), Some(PathBuf::from("a/b/c")));
        assert_eq!(
            safe_relative("/p/some%20id"),
            Some(PathBuf::from("p/some id"))
        );
        assert_eq!(
            safe_relative("/_app/immutable/a.js"),
            Some(PathBuf::from("_app/immutable/a.js"))
        );
    }
}
