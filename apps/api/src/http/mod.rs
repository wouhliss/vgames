//! Router assembly and cross-cutting layers.
//!
//! Layer order, outermost first (docs/agents/agent-1-backend.md A1-T01): request id →
//! trace → catch panic → timeout (30 s, not on `/v1/realtime`) → body limit (1 MiB) →
//! gzip → security headers.

pub mod client_ip;
pub mod context;
pub mod etag;
pub mod idempotency;
pub mod json;
pub mod pagination;
pub mod query;
pub mod ratelimit;
pub mod security_headers;

use std::{any::Any, time::Duration};

use axum::{
    Router,
    extract::{DefaultBodyLimit, Request},
    http::header,
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use tower::ServiceBuilder;
use tower_http::{
    catch_panic::CatchPanicLayer,
    compression::CompressionLayer,
    sensitive_headers::{SetSensitiveRequestHeadersLayer, SetSensitiveResponseHeadersLayer},
    trace::TraceLayer,
};

use utoipa_axum::router::OpenApiRouter;

use crate::{error::ApiError, state::AppState};

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_BODY_LIMIT: usize = 1024 * 1024;

/// All documented routes. Modules contribute `OpenApiRouter`s so the served OpenAPI
/// document is generated from the handlers themselves (drift-checked against
/// `openapi/openapi.yaml` by `tests/openapi_contract.rs`).
pub fn api_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::with_openapi(crate::openapi::base_document())
        .merge(crate::discovery::routes())
        .merge(crate::auth::routes())
        .merge(crate::realtime::routes())
        .merge(crate::packages::routes())
        .merge(crate::assets::routes())
        .merge(crate::metadata::routes())
        .merge(crate::trust::routes())
        .merge(crate::versions::routes())
        .merge(crate::finalize::routes())
        .merge(crate::releases::routes())
        .merge(crate::downloads::routes())
        .merge(crate::compat_profiles::routes())
        .merge(crate::saves::routes())
        .merge(crate::users::routes())
        .merge(crate::admin::routes())
        .merge(crate::social::routes())
}

/// The OpenAPI document generated from the handlers.
pub fn openapi() -> utoipa::openapi::OpenApi {
    crate::openapi::finalize(api_routes().split_for_parts().1)
}

/// Builds the complete application router.
pub fn router(state: AppState) -> Router {
    let (api, doc) = api_routes().split_for_parts();
    let doc = crate::openapi::finalize(doc);
    let routes = api
        .merge(crate::openapi::docs_routes(doc))
        .merge(crate::auth::dev_routes(&state))
        .merge(crate::storage::fs::routes(&state));
    with_layers(routes, state)
}

/// Applies the fallbacks and the cross-cutting layer stack to `routes`.
pub fn with_layers(routes: Router<AppState>, state: AppState) -> Router {
    routes
        .layer(middleware::from_fn(timeout))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(
            ServiceBuilder::new()
                .layer(middleware::from_fn(context::request_context))
                .layer(middleware::from_fn_with_state(
                    state.clone(),
                    ratelimit::ip_limits,
                ))
                .layer(SetSensitiveRequestHeadersLayer::new([
                    header::AUTHORIZATION,
                    header::COOKIE,
                ]))
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(trace_span)
                        .on_request(())
                        .on_response(
                            tower_http::trace::DefaultOnResponse::new().level(tracing::Level::INFO),
                        ),
                )
                .layer(SetSensitiveResponseHeadersLayer::new([
                    header::SET_COOKIE,
                    header::LOCATION,
                ]))
                .layer(CatchPanicLayer::custom(panic_response))
                .layer(DefaultBodyLimit::max(DEFAULT_BODY_LIMIT))
                .layer(CompressionLayer::new().gzip(true))
                .layer(middleware::from_fn_with_state(
                    state.clone(),
                    security_headers::security_headers,
                )),
        )
        .with_state(state)
}

/// Request span: method, path (never the query string) and request id.
fn trace_span(req: &Request) -> tracing::Span {
    let request_id = req
        .extensions()
        .get::<context::RequestContext>()
        .map(|c| c.request_id.as_str())
        .unwrap_or("");
    tracing::info_span!("http", method = %req.method(), path = %req.uri().path(), request_id = %request_id)
}

/// Paths exempt from the request timeout (long-lived WebSocket).
const NO_TIMEOUT_PATHS: &[&str] = &["/v1/realtime"];

async fn timeout(req: Request, next: Next) -> Response {
    if NO_TIMEOUT_PATHS.contains(&req.uri().path()) {
        return next.run(req).await;
    }
    match tokio::time::timeout(REQUEST_TIMEOUT, next.run(req)).await {
        Ok(resp) => resp,
        Err(_) => ApiError::timeout().into_response(),
    }
}

fn panic_response(err: Box<dyn Any + Send + 'static>) -> Response {
    let msg = err
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| err.downcast_ref::<&str>().copied())
        .unwrap_or("unknown panic");
    tracing::error!(panic = msg, "handler panicked");
    ApiError::internal().into_response()
}

async fn not_found() -> ApiError {
    ApiError::not_found()
}

async fn method_not_allowed() -> ApiError {
    ApiError::new(
        axum::http::StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "Method not allowed",
    )
}
