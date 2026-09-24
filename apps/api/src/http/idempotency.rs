//! `Idempotency-Key` support for create endpoints (docs/architecture/03-api.md §1).
//!
//! The first request with a key runs the handler and stores its response; a replay with
//! the same key and the same request returns the stored response (with
//! `Idempotent-Replayed: true`); the same key with a different request is
//! `409 idempotency_key_reused`; a replay while the first is still running is
//! `409 idempotency_in_progress`. Keys expire after 24 hours. Failed requests release
//! their key so the client can retry.

use axum::{
    extract::FromRequestParts,
    http::{HeaderValue, StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};

pub const HEADER: &str = "idempotency-key";

/// The optional `Idempotency-Key` header, validated (`[A-Za-z0-9_-]{16,128}`).
#[derive(Debug, Clone)]
pub struct IdempotencyKey(pub Option<String>);

impl<S: Send + Sync> FromRequestParts<S> for IdempotencyKey {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let Some(v) = parts.headers.get(HEADER) else {
            return Ok(IdempotencyKey(None));
        };
        let key = v.to_str().unwrap_or_default();
        let valid = (16..=128).contains(&key.len())
            && key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !valid {
            return Err(ApiError::field(
                "Idempotency-Key",
                "invalid",
                "must be 16-128 characters of A-Z, a-z, 0-9, - and _",
            ));
        }
        Ok(IdempotencyKey(Some(key.to_string())))
    }
}

/// A stored or fresh JSON response.
pub struct StoredResponse {
    pub status: StatusCode,
    pub body: serde_json::Value,
    pub replayed: bool,
    pub headers: Vec<(axum::http::HeaderName, HeaderValue)>,
}

impl IntoResponse for StoredResponse {
    fn into_response(self) -> Response {
        let mut resp = (self.status, axum::Json(self.body)).into_response();
        for (k, v) in self.headers {
            resp.headers_mut().insert(k, v);
        }
        if self.replayed {
            resp.headers_mut()
                .insert("idempotent-replayed", HeaderValue::from_static("true"));
        }
        resp
    }
}

/// SHA-256 over method, path and the canonical JSON of the parsed request body.
pub fn fingerprint<B: Serialize>(method: &str, path: &str, body: &B) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(method.as_bytes());
    h.update(b"\n");
    h.update(path.as_bytes());
    h.update(b"\n");
    h.update(serde_json::to_vec(body).unwrap_or_default());
    h.finalize().into()
}

/// Runs `handler` at most once per `(user, key)`.
///
/// `handler` returns the status and JSON body to store; errors are returned as-is and
/// release the key.
pub async fn run<F, Fut>(
    db: &PgPool,
    user_id: Uuid,
    key: &IdempotencyKey,
    fingerprint: [u8; 32],
    handler: F,
) -> ApiResult<StoredResponse>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ApiResult<(StatusCode, serde_json::Value)>>,
{
    let Some(key) = &key.0 else {
        let (status, body) = handler().await?;
        return Ok(StoredResponse {
            status,
            body,
            replayed: false,
            headers: Vec::new(),
        });
    };

    sqlx::query!(
        "DELETE FROM idempotency_keys WHERE user_id = $1 AND key = $2 AND expires_at <= now()",
        user_id,
        key
    )
    .execute(db)
    .await?;

    let claimed = sqlx::query_scalar!(
        r#"INSERT INTO idempotency_keys (user_id, key, request_fingerprint)
           VALUES ($1, $2, $3)
           ON CONFLICT (user_id, key) DO NOTHING
           RETURNING 1 AS "one!""#,
        user_id,
        key,
        &fingerprint[..]
    )
    .fetch_optional(db)
    .await?
    .is_some();

    if !claimed {
        let row = sqlx::query!(
            "SELECT request_fingerprint, status_code, response_body FROM idempotency_keys WHERE user_id = $1 AND key = $2",
            user_id,
            key
        )
        .fetch_optional(db)
        .await?
        .ok_or_else(|| ApiError::conflict("idempotency_in_progress", "A request with this Idempotency-Key is still running"))?;
        if row.request_fingerprint != fingerprint {
            return Err(ApiError::conflict(
                "idempotency_key_reused",
                "This Idempotency-Key was already used for a different request",
            ));
        }
        return match (row.status_code, row.response_body) {
            (Some(code), Some(body)) => Ok(StoredResponse {
                status: StatusCode::from_u16(code as u16).map_err(ApiError::internal_from)?,
                body,
                replayed: true,
                headers: Vec::new(),
            }),
            _ => Err(ApiError::conflict(
                "idempotency_in_progress",
                "A request with this Idempotency-Key is still running",
            )),
        };
    }

    match handler().await {
        Ok((status, body)) => {
            sqlx::query!(
                "UPDATE idempotency_keys SET status_code = $3, response_body = $4 WHERE user_id = $1 AND key = $2",
                user_id,
                key,
                status.as_u16() as i16,
                &body
            )
            .execute(db)
            .await?;
            Ok(StoredResponse {
                status,
                body,
                replayed: false,
                headers: Vec::new(),
            })
        }
        Err(e) => {
            sqlx::query!(
                "DELETE FROM idempotency_keys WHERE user_id = $1 AND key = $2",
                user_id,
                key
            )
            .execute(db)
            .await?;
            Err(e)
        }
    }
}
