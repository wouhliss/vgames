//! JSON body extraction with problem+json errors and field paths.
//!
//! - Content type must be `application/json` (or `application/merge-patch+json` for PATCH).
//! - Unknown fields are rejected (`400 unknown_field`) via `deny_unknown_fields` on DTOs.
//! - Type errors become `400 validation_failed` with the offending field path.
//! - After deserializing, [`Validate::validate`] enforces the schema's limits.
//! - The 1 MiB body limit (`DefaultBodyLimit`) becomes `413 payload_too_large`.

use axum::{
    extract::{FromRequest, Request, rejection::BytesRejection},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use serde::de::DeserializeOwned;
use vgames_proto::FieldError;

use crate::error::ApiError;

/// Semantic validation run after deserialization. Collect *all* problems.
pub trait Validate {
    fn validate(&self, errors: &mut Vec<FieldError>);
}

/// Pushes a field error.
pub fn invalid(errors: &mut Vec<FieldError>, field: &str, code: &str, message: impl Into<String>) {
    errors.push(FieldError {
        field: field.to_string(),
        code: code.to_string(),
        message: Some(message.into()),
    });
}

/// Validated JSON body.
pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned + Validate,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        if !is_json_content_type(req.headers()) {
            return Err(
                ApiError::unsupported_media_type().with_detail("Send the body as application/json")
            );
        }
        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(map_bytes_rejection)?;
        let value = parse_json::<T>(&bytes)?;
        let mut errors = Vec::new();
        value.validate(&mut errors);
        if !errors.is_empty() {
            return Err(ApiError::validation(errors));
        }
        Ok(Json(value))
    }
}

/// Serializes `T` as a JSON response with the given status.
pub struct JsonResponse<T>(pub StatusCode, pub T);

impl<T: serde::Serialize> IntoResponse for JsonResponse<T> {
    fn into_response(self) -> Response {
        (self.0, axum::Json(self.1)).into_response()
    }
}

pub fn is_json_content_type(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            let mime = v
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            mime == "application/json" || mime == "application/merge-patch+json"
        })
        .unwrap_or(false)
}

fn map_bytes_rejection(r: BytesRejection) -> ApiError {
    if r.status() == StatusCode::PAYLOAD_TOO_LARGE {
        ApiError::payload_too_large()
    } else {
        ApiError::bad_request("invalid_body", "The request body could not be read")
    }
}

/// Deserializes with a field path in errors.
pub fn parse_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ApiError> {
    let de = &mut serde_json::Deserializer::from_slice(bytes);
    serde_path_to_error::deserialize(de).map_err(|e| {
        let path = e.path().to_string();
        let inner = e.inner();
        let msg = inner.to_string();
        if inner.is_syntax() || inner.is_eof() {
            return ApiError::bad_request("malformed_json", "The request body is not valid JSON")
                .with_detail(msg);
        }
        if let Some(field) = unknown_field_name(&msg) {
            // The path already ends with the unknown field's name.
            let full = if path == "." || path.is_empty() {
                field
            } else {
                path
            };
            return ApiError::bad_request("unknown_field", "The request contains an unknown field")
                .with_errors(vec![FieldError {
                    field: full,
                    code: "unknown_field".into(),
                    message: Some(msg),
                }]);
        }
        let field = if path == "." || path.is_empty() {
            missing_field_name(&msg).unwrap_or_default()
        } else {
            path
        };
        ApiError::validation(vec![FieldError {
            field,
            code: "invalid".into(),
            message: Some(strip_position(&msg)),
        }])
    })
}

fn unknown_field_name(msg: &str) -> Option<String> {
    let rest = msg.strip_prefix("unknown field `")?;
    Some(rest.split('`').next()?.to_string())
}

fn missing_field_name(msg: &str) -> Option<String> {
    let rest = msg.strip_prefix("missing field `")?;
    Some(rest.split('`').next()?.to_string())
}

fn strip_position(msg: &str) -> String {
    match msg.find(" at line ") {
        Some(i) => msg[..i].to_string(),
        None => msg.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Body {
        name: String,
        inner: Inner,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Inner {
        size: u32,
    }

    #[test]
    fn unknown_fields_are_reported_with_their_path() {
        let e = parse_json::<Body>(br#"{"name":"x","inner":{"size":1,"sise":2}}"#).unwrap_err();
        assert_eq!(e.code, "unknown_field");
        assert_eq!(e.errors[0].field, "inner.sise");
    }

    #[test]
    fn type_errors_name_the_field() {
        let e = parse_json::<Body>(br#"{"name":"x","inner":{"size":"big"}}"#).unwrap_err();
        assert_eq!(e.code, "validation_failed");
        assert_eq!(e.errors[0].field, "inner.size");
        let e = parse_json::<Body>(br#"{"inner":{"size":1}}"#).unwrap_err();
        assert_eq!(e.errors[0].field, "name");
    }

    #[test]
    fn malformed_json_is_distinct() {
        assert_eq!(
            parse_json::<Body>(b"{nope").unwrap_err().code,
            "malformed_json"
        );
    }
}
