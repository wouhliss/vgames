//! Query-string extraction with problem+json errors.

use axum::{
    extract::{FromRequestParts, Query as AxumQuery},
    http::request::Parts,
};
use serde::de::DeserializeOwned;

use crate::{error::ApiError, http::json::Validate};

/// Validated query string. Unknown parameters are rejected when `T` uses `deny_unknown_fields`.
pub struct Query<T>(pub T);

impl<T, S> FromRequestParts<S> for Query<T>
where
    T: DeserializeOwned + Validate,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let AxumQuery(value) = AxumQuery::<T>::from_request_parts(parts, state)
            .await
            .map_err(|e| {
                ApiError::bad_request("invalid_query", "The query string is invalid")
                    .with_detail(e.body_text())
            })?;
        let mut errors = Vec::new();
        value.validate(&mut errors);
        if !errors.is_empty() {
            return Err(ApiError::validation(errors));
        }
        Ok(Query(value))
    }
}
