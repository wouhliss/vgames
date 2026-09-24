//! RFC 9457 problem details. Every error the API returns goes through [`ApiError`].
//!
//! `type` is `urn:vgames:problem:<code>`, `code` is the stable identifier clients switch
//! on, `instance` is the request path and `request_id` correlates with server logs.
//! Internals (SQL, bucket names, stack traces) never reach `detail`.

use std::borrow::Cow;

use axum::{
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use vgames_proto::{FieldError, Problem};

use crate::http::context;

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: Cow<'static, str>,
    pub title: Cow<'static, str>,
    pub detail: Option<String>,
    pub errors: Vec<FieldError>,
    pub retry_after_secs: Option<u64>,
}

impl ApiError {
    pub fn new(
        status: StatusCode,
        code: impl Into<Cow<'static, str>>,
        title: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            status,
            code: code.into(),
            title: title.into(),
            detail: None,
            errors: Vec::new(),
            retry_after_secs: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_errors(mut self, errors: Vec<FieldError>) -> Self {
        self.errors = errors;
        self
    }

    // ---- common problems (03-api §2) -------------------------------------------------------

    pub fn validation(errors: Vec<FieldError>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "validation_failed",
            "The request is invalid",
        )
        .with_errors(errors)
    }

    pub fn field(
        field: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::validation(vec![FieldError {
            field: field.into(),
            code: code.into(),
            message: Some(message.into()),
        }])
    }

    pub fn bad_request(
        code: impl Into<Cow<'static, str>>,
        title: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, title)
    }

    pub fn unauthenticated() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "Sign in to continue",
        )
    }

    pub fn session_expired() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "session_expired",
            "Your session has expired",
        )
    }

    pub fn forbidden() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "You are not allowed to do this",
        )
    }

    pub fn forbidden_code(
        code: impl Into<Cow<'static, str>>,
        title: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, title)
    }

    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "Not found")
    }

    pub fn conflict(
        code: impl Into<Cow<'static, str>>,
        title: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self::new(StatusCode::CONFLICT, code, title)
    }

    pub fn gone(code: impl Into<Cow<'static, str>>, title: impl Into<Cow<'static, str>>) -> Self {
        Self::new(StatusCode::GONE, code, title)
    }

    pub fn precondition_failed() -> Self {
        Self::new(
            StatusCode::PRECONDITION_FAILED,
            "precondition_failed",
            "This resource was changed by someone else; reload it and try again",
        )
    }

    pub fn precondition_required() -> Self {
        Self::new(
            StatusCode::PRECONDITION_REQUIRED,
            "precondition_required",
            "An If-Match header is required",
        )
    }

    pub fn payload_too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "The request body is too large",
        )
    }

    pub fn unsupported_media_type() -> Self {
        Self::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "Unsupported content type",
        )
    }

    pub fn unprocessable(
        code: impl Into<Cow<'static, str>>,
        title: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, code, title)
    }

    pub fn rate_limited(retry_after_secs: u64) -> Self {
        let mut e = Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Too many requests",
        );
        e.retry_after_secs = Some(retry_after_secs.max(1));
        e
    }

    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "Something went wrong on the server",
        )
    }

    pub fn unavailable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "The service is temporarily unavailable",
        )
    }

    pub fn timeout() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "timeout",
            "The server took too long to respond",
        )
    }

    /// Logs `err` with the request id and returns a generic 500.
    pub fn internal_from(err: impl std::fmt::Display) -> Self {
        tracing::error!(error = %err, "internal error");
        Self::internal()
    }

    pub fn to_problem(&self) -> Problem {
        let ctx = context::current();
        Problem {
            kind: format!("urn:vgames:problem:{}", self.code),
            title: self.title.to_string(),
            status: self.status.as_u16(),
            code: self.code.to_string(),
            detail: self.detail.clone(),
            instance: ctx.as_ref().map(|c| c.path.clone()),
            request_id: ctx.map(|c| c.request_id),
            errors: self.errors.clone(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.status.as_u16(), self.code)
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::to_vec(&self.to_problem()).unwrap_or_else(|_| b"{}".to_vec());
        let mut resp = (self.status, body).into_response();
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        if let Some(secs) = self.retry_after_secs
            && let Ok(v) = HeaderValue::from_str(&secs.to_string())
        {
            resp.headers_mut().insert(header::RETRY_AFTER, v);
        }
        resp
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        match &err {
            sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => {
                tracing::error!(error = %err, "database unavailable");
                Self::unavailable()
            }
            _ => Self::internal_from(err),
        }
    }
}

/// Returns `true` when `err` is a Postgres unique violation (optionally on `constraint`).
pub fn is_unique_violation(err: &sqlx::Error, constraint: Option<&str>) -> bool {
    match err {
        sqlx::Error::Database(db) => {
            db.code().as_deref() == Some("23505")
                && constraint.is_none_or(|c| db.constraint() == Some(c))
        }
        _ => false,
    }
}
