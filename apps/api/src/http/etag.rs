//! Optimistic concurrency with ETag / If-Match (docs/architecture/03-api.md §1).
//!
//! ETags are weak (`W/"<updated_at in µs>"`) and compared weakly: they identify a
//! version of the resource as stored, which is exactly what concurrent edits need.

use axum::{
    extract::FromRequestParts,
    http::{HeaderValue, header, request::Parts},
};
use time::OffsetDateTime;

use crate::error::ApiError;

/// Weak ETag for a row version.
pub fn etag(updated_at: OffsetDateTime) -> String {
    let micros = updated_at.unix_timestamp_nanos() / 1_000;
    format!("W/\"{micros}\"")
}

pub fn etag_header(updated_at: OffsetDateTime) -> (header::HeaderName, HeaderValue) {
    let v = HeaderValue::from_str(&etag(updated_at))
        .unwrap_or_else(|_| HeaderValue::from_static("W/\"0\""));
    (header::ETAG, v)
}

/// The raw `If-Match` header, if present.
pub struct IfMatch(pub Option<String>);

impl<S: Send + Sync> FromRequestParts<S> for IfMatch {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        match parts.headers.get(header::IF_MATCH) {
            None => Ok(IfMatch(None)),
            Some(v) => v
                .to_str()
                .map(|s| IfMatch(Some(s.trim().to_string())))
                .map_err(|_| {
                    ApiError::bad_request("invalid_if_match", "The If-Match header is invalid")
                }),
        }
    }
}

impl IfMatch {
    /// `428` without the header, `412` when no listed tag matches `current`.
    pub fn check(&self, current: &str) -> Result<(), ApiError> {
        let Some(header) = &self.0 else {
            return Err(ApiError::precondition_required());
        };
        if header == "*" {
            return Ok(());
        }
        let want = opaque(current);
        if header.split(',').map(str::trim).any(|t| opaque(t) == want) {
            Ok(())
        } else {
            Err(ApiError::precondition_failed())
        }
    }
}

fn opaque(tag: &str) -> &str {
    tag.strip_prefix("W/").unwrap_or(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks() {
        let t = OffsetDateTime::from_unix_timestamp_nanos(1_700_000_000_123_456_789).unwrap();
        let tag = etag(t);
        assert_eq!(tag, "W/\"1700000000123456\"");
        assert_eq!(
            IfMatch(None).check(&tag).unwrap_err().code,
            "precondition_required"
        );
        assert_eq!(
            IfMatch(Some("W/\"1\"".into()))
                .check(&tag)
                .unwrap_err()
                .code,
            "precondition_failed"
        );
        assert!(IfMatch(Some(tag.clone())).check(&tag).is_ok());
        assert!(
            IfMatch(Some("\"1700000000123456\"".into()))
                .check(&tag)
                .is_ok()
        );
        assert!(IfMatch(Some(format!("W/\"9\", {tag}"))).check(&tag).is_ok());
        assert!(IfMatch(Some("*".into())).check(&tag).is_ok());
    }
}
