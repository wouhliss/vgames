//! Path parameters with problem+json errors.
//!
//! axum's own `Path` rejection is plain text. A path parameter that does not parse (a
//! malformed UUID, an unknown platform) is `400 invalid_path`.

use axum::{
    extract::{FromRequestParts, rejection::PathRejection},
    http::request::Parts,
};
use serde::de::DeserializeOwned;

use crate::error::ApiError;

pub struct Path<T>(pub T);

impl<T, S> FromRequestParts<S> for Path<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Path::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Path(value)) => Ok(Path(value)),
            Err(PathRejection::FailedToDeserializePathParams(e)) => Err(ApiError::bad_request(
                "invalid_path",
                "The URL contains an invalid identifier",
            )
            .with_detail(e.body_text())),
            Err(e) => Err(ApiError::internal_from(e)),
        }
    }
}
