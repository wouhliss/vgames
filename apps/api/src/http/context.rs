//! Per-request context (request id + path), available to error rendering anywhere in
//! the request's task through a task-local.

use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};

pub static REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Clone, Debug)]
pub struct RequestContext {
    pub request_id: String,
    pub path: String,
}

tokio::task_local! {
    static CONTEXT: RequestContext;
}

/// The current request's context, when called inside a request.
pub fn current() -> Option<RequestContext> {
    CONTEXT.try_with(Clone::clone).ok()
}

/// Accepts a client-sent `X-Request-Id` when it is 8–64 chars of `[A-Za-z0-9_-]`,
/// otherwise generates a UUIDv7. Echoes it on the response.
pub async fn request_context(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(&REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| valid_request_id(v))
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    let ctx = RequestContext {
        request_id: id.clone(),
        path: req.uri().path().to_string(),
    };
    req.extensions_mut().insert(ctx.clone());
    let mut resp = CONTEXT.scope(ctx, next.run(req)).await;
    if let Ok(v) = HeaderValue::from_str(&id) {
        resp.headers_mut().insert(REQUEST_ID_HEADER.clone(), v);
    }
    resp
}

fn valid_request_id(v: &str) -> bool {
    (8..=64).contains(&v.len())
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    #[test]
    fn request_id_validation() {
        assert!(super::valid_request_id("01J9ZX7Q3K-abc_def"));
        assert!(!super::valid_request_id("short"));
        assert!(!super::valid_request_id("has space in it!"));
        assert!(!super::valid_request_id(&"a".repeat(65)));
    }
}
