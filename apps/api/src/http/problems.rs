//! Every error the API answers is problem+json (03-api §2), including the framework's own
//! rejections: axum's path, query, form and WebSocket extractors answer plain text. This
//! layer rewrites any non-JSON error body on `/v1/*` and `/.well-known/*` into a problem
//! with the same status, keeping the original text as `detail`. JSON bodies are left alone
//! (`GET /v1/health` answers 503 with its `Health` document).

use axum::{
    extract::Request,
    http::{HeaderName, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::error::ApiError;

/// Largest error body read back (framework rejections are one line).
const MAX_BODY: usize = 4096;
const MAX_DETAIL_CHARS: usize = 300;
/// Response headers that stay meaningful on the rewritten problem.
const KEEP: [HeaderName; 3] = [header::ALLOW, header::RETRY_AFTER, header::WWW_AUTHENTICATE];

fn is_api(path: &str) -> bool {
    path.starts_with("/v1/") || path.starts_with("/.well-known/")
}

/// The problem code and title for an error that did not come from [`ApiError`].
pub fn code_for(status: StatusCode) -> (&'static str, &'static str) {
    match status.as_u16() {
        400 => ("bad_request", "The request is invalid"),
        401 => ("unauthenticated", "Sign in to continue"),
        403 => ("forbidden", "You are not allowed to do this"),
        404 => ("not_found", "Not found"),
        405 => ("method_not_allowed", "Method not allowed"),
        409 => ("conflict", "The request conflicts with the current state"),
        413 => ("payload_too_large", "The request body is too large"),
        415 => ("unsupported_media_type", "Unsupported content type"),
        422 => ("unprocessable", "The request cannot be processed"),
        426 => (
            "upgrade_required",
            "This endpoint needs a WebSocket upgrade",
        ),
        429 => ("rate_limited", "Too many requests"),
        503 => ("unavailable", "The service is temporarily unavailable"),
        s if s >= 500 => ("internal", "Something went wrong on the server"),
        _ => ("request_failed", "The request failed"),
    }
}

pub async fn normalize(req: Request, next: Next) -> Response {
    let api = is_api(req.uri().path());
    let resp = next.run(req).await;
    let status = resp.status();
    if !api || !(status.is_client_error() || status.is_server_error()) {
        return resp;
    }
    let is_json = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| {
            ct.starts_with("application/problem+json") || ct.starts_with("application/json")
        });
    if is_json {
        return resp;
    }
    let (parts, body) = resp.into_parts();
    let text: String = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes)
            .trim()
            .chars()
            .take(MAX_DETAIL_CHARS)
            .collect(),
        Err(_) => String::new(),
    };
    let (code, title) = code_for(status);
    let mut problem = ApiError::new(status, code, title);
    if !text.is_empty() {
        problem = problem.with_detail(text);
    }
    let mut out = problem.into_response();
    for name in KEEP {
        if let Some(v) = parts.headers.get(&name) {
            out.headers_mut().insert(name, v.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_the_problem_pattern() {
        for s in 400..600u16 {
            let (code, title) =
                code_for(StatusCode::from_u16(s).unwrap_or(StatusCode::IM_A_TEAPOT));
            assert!(
                code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{code}"
            );
            assert!(!title.is_empty());
        }
    }
}
