//! Hosts the built admin SPA (`VGAMES_ADMIN_DIST`) at `/admin/` (A1-T15, 01-security §5).
//!
//! - `/admin` redirects to `/admin/`; `/admin/` and unknown client routes serve `index.html`
//!   with `Cache-Control: no-store`, so a deploy is picked up on the next load.
//! - Files under `assets/` carry content hashes in their names (Vite) and are served
//!   `public, max-age=31536000, immutable`; other files are `no-cache`.
//! - Every response carries the admin CSP.
//! - Request paths never leave the dist directory: `.`/`..`/empty/hidden segments,
//!   backslashes and colons are refused, and the resolved file (after symlinks) must
//!   still be inside the canonical root.
//!
//! Not part of the OpenAPI document.

use std::path::{Path as FsPath, PathBuf};

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Redirect, Response},
    routing::get,
};
use tokio_util::io::ReaderStream;

use crate::{error::ApiError, state::AppState};

/// The admin CSP (01-security §5).
pub const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self'; \
connect-src 'self' https://storage.googleapis.com; img-src 'self' data: https://storage.googleapis.com; \
style-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const MAX_PATH: usize = 512;

pub fn routes(state: &AppState) -> Router<AppState> {
    if state.config.admin_dist.is_none() {
        return Router::new();
    }
    Router::new()
        .route("/admin", get(|| async { Redirect::permanent("/admin/") }))
        .route("/admin/", get(index))
        .route("/admin/{*path}", get(file))
}

/// Refuses anything that is not a plain relative path of visible segments.
pub fn safe_relative(path: &str) -> Option<&str> {
    let ok = !path.is_empty()
        && path.len() <= MAX_PATH
        && !path.contains(['\\', ':', '\0'])
        && path
            .split('/')
            .all(|seg| !seg.is_empty() && !seg.starts_with('.'));
    ok.then_some(path)
}

fn content_type(path: &FsPath) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

async fn serve(path: &FsPath, cache: &'static str) -> Result<Response, ApiError> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| ApiError::not_found())?;
    let len = file
        .metadata()
        .await
        .map_err(ApiError::internal_from)?
        .len();
    let mut resp = Body::from_stream(ReaderStream::new(file)).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(path)),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    Ok(resp)
}

fn root(state: &AppState) -> Result<&PathBuf, ApiError> {
    state
        .config
        .admin_dist
        .as_ref()
        .ok_or_else(ApiError::not_found)
}

async fn index(State(state): State<AppState>) -> Result<Response, ApiError> {
    serve(&root(&state)?.join("index.html"), "no-store").await
}

async fn file(
    State(state): State<AppState>,
    Path(path): Path<String>,
) -> Result<Response, ApiError> {
    let root = root(&state)?;
    let rel = safe_relative(&path).ok_or_else(ApiError::not_found)?;
    match tokio::fs::canonicalize(root.join(rel)).await {
        Ok(resolved) => {
            // Symlinks may not point outside the dist directory.
            if !resolved.starts_with(root) || !resolved.is_file() {
                return Err(ApiError::not_found());
            }
            let cache = if rel == "index.html" {
                "no-store"
            } else if rel.starts_with("assets/") {
                IMMUTABLE
            } else {
                "no-cache"
            };
            serve(&resolved, cache).await
        }
        // A missing file (name with an extension) is a 404; anything else is a client-side
        // route of the SPA.
        Err(_)
            if rel
                .rsplit('/')
                .next()
                .is_some_and(|last| last.contains('.')) =>
        {
            Err(ApiError::not_found())
        }
        Err(_) => serve(&root.join("index.html"), "no-store").await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_relative_paths_pass() {
        for ok in ["index.html", "assets/app-3f2a.js", "users/42", "a/b/c.wasm"] {
            assert_eq!(safe_relative(ok), Some(ok), "{ok}");
        }
        for bad in [
            "",
            "../secret",
            "a/../../b",
            "./a",
            "a//b",
            "/etc/passwd",
            ".env",
            "assets/.hidden",
            "a\\..\\b",
            "C:/x",
            "a\0b",
            "a/",
            &"a".repeat(513),
        ] {
            assert_eq!(safe_relative(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn content_types() {
        assert_eq!(content_type(FsPath::new("x.WASM")), "application/wasm");
        assert_eq!(
            content_type(FsPath::new("a/b.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type(FsPath::new("noext")),
            "application/octet-stream"
        );
    }
}
