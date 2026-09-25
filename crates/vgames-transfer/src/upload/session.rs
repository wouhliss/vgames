//! The resumable upload protocol of Google Cloud Storage (and the API's `fs`
//! backend, which mirrors it; 02 §6 step 3):
//!
//! - start: `POST <signed url>` with exactly the signed headers
//!   (`x-goog-resumable: start`, …) → `200`/`201` + `Location` = session URI;
//! - data: `PUT <session>` with `Content-Range: bytes a-b/total` →
//!   `308` + `Range: bytes=0-n` (persisted so far) until complete, then `200`/`201`;
//! - status: `PUT <session>` with an empty body and `Content-Range: bytes */total`.
//!
//! Session URIs are credentials: they are never logged.

use std::collections::BTreeMap;
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, LOCATION, RANGE};

use crate::http::redact_url;

/// What the storage server has of an object being uploaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// Every byte is stored; the object exists.
    Complete,
    /// The first `n` bytes are stored.
    Incomplete(u64),
}

/// Why a session request failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    /// Network error, timeout, `408`, `429` or `5xx`: query the status and retry.
    #[error("transient upload failure: {0}")]
    Transient(String),
    /// `404`/`410`: the session expired or was lost; start a new one.
    #[error("the upload session is gone (HTTP {0})")]
    Gone(u16),
    /// The storage server refused the request (for example an expired start URL).
    #[error("the storage server refused the upload (HTTP {status}): {detail}")]
    Refused { status: u16, detail: String },
    #[error("the storage server answered unexpectedly: {0}")]
    Protocol(String),
}

impl SessionError {
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }

    fn from_reqwest(error: reqwest::Error) -> Self {
        Self::Transient(error.without_url().to_string())
    }
}

fn classify(status: StatusCode, detail: String) -> SessionError {
    match status.as_u16() {
        404 | 410 => SessionError::Gone(status.as_u16()),
        408 | 429 => SessionError::Transient(format!("HTTP {status}")),
        s if s >= 500 => SessionError::Transient(format!("HTTP {status}")),
        s => SessionError::Refused { status: s, detail },
    }
}

async fn body_snippet(response: reqwest::Response) -> String {
    let text = response.text().await.unwrap_or_default();
    text.chars().take(200).collect()
}

/// Starts a session with a signed start request; returns the session URI.
pub async fn start(
    client: &reqwest::Client,
    url: &str,
    headers: &BTreeMap<String, String>,
    timeout: Duration,
) -> Result<String, SessionError> {
    let mut request = client.post(url).header(CONTENT_LENGTH, "0");
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }
    let response = tokio::time::timeout(timeout, request.send())
        .await
        .map_err(|_| SessionError::Transient("no response in time".into()))?
        .map_err(SessionError::from_reqwest)?;
    let status = response.status();
    if status != StatusCode::OK && status != StatusCode::CREATED {
        let detail = body_snippet(response).await;
        return Err(classify(status, detail));
    }
    let location = response
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .filter(|l| l.starts_with("https://") || l.starts_with("http://"))
        .ok_or_else(|| {
            SessionError::Protocol(format!("no session URI from {}", redact_url(url)))
        })?;
    Ok(location.to_owned())
}

fn parse_status(response: &reqwest::Response) -> Result<SessionStatus, SessionError> {
    match response.status() {
        StatusCode::OK | StatusCode::CREATED => Ok(SessionStatus::Complete),
        StatusCode::PERMANENT_REDIRECT => {
            let Some(range) = response.headers().get(RANGE) else {
                return Ok(SessionStatus::Incomplete(0));
            };
            let range = range
                .to_str()
                .map_err(|_| SessionError::Protocol("unreadable Range header".into()))?;
            let end = range
                .strip_prefix("bytes=0-")
                .and_then(|n| n.parse::<u64>().ok())
                .ok_or_else(|| SessionError::Protocol(format!("unexpected Range {range:?}")))?;
            Ok(SessionStatus::Incomplete(end + 1))
        }
        other => Err(classify(other, String::new())),
    }
}

/// Asks how many bytes the server has.
pub async fn query(
    client: &reqwest::Client,
    session: &str,
    total: u64,
    timeout: Duration,
) -> Result<SessionStatus, SessionError> {
    let request = client
        .put(session)
        .header(CONTENT_LENGTH, "0")
        .header(CONTENT_RANGE, format!("bytes */{total}"));
    let response = tokio::time::timeout(timeout, request.send())
        .await
        .map_err(|_| SessionError::Transient("no response in time".into()))?
        .map_err(SessionError::from_reqwest)?;
    let status = parse_status(&response);
    if let Err(SessionError::Refused { status, .. }) = status {
        let detail = body_snippet(response).await;
        return Err(SessionError::Refused { status, detail });
    }
    status
}

/// Sends `piece` as bytes `offset..offset + len` of an object of `total` bytes.
pub async fn put_piece(
    client: &reqwest::Client,
    session: &str,
    offset: u64,
    piece: bytes::Bytes,
    total: u64,
    timeout: Duration,
) -> Result<SessionStatus, SessionError> {
    let len = piece.len() as u64;
    if len == 0 || offset + len > total {
        return Err(SessionError::Protocol("piece outside the object".into()));
    }
    let request = client
        .put(session)
        .header(CONTENT_LENGTH, len.to_string())
        .header(
            CONTENT_RANGE,
            format!("bytes {offset}-{}/{total}", offset + len - 1),
        )
        .body(piece);
    let response = tokio::time::timeout(timeout, request.send())
        .await
        .map_err(|_| SessionError::Transient("the piece did not finish in time".into()))?
        .map_err(SessionError::from_reqwest)?;
    let status = parse_status(&response);
    if let Err(SessionError::Refused { status, .. }) = status {
        let detail = body_snippet(response).await;
        return Err(SessionError::Refused { status, detail });
    }
    status
}

/// A single signed `PUT` of a whole object (the manifest).
pub async fn put_object(
    client: &reqwest::Client,
    url: &str,
    headers: &BTreeMap<String, String>,
    body: bytes::Bytes,
    timeout: Duration,
) -> Result<(), SessionError> {
    let mut request = client
        .put(url)
        .header(CONTENT_LENGTH, body.len().to_string());
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }
    let response = tokio::time::timeout(timeout, request.body(body).send())
        .await
        .map_err(|_| SessionError::Transient("no response in time".into()))?
        .map_err(SessionError::from_reqwest)?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let detail = body_snippet(response).await;
    Err(classify(status, detail))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_are_classified() {
        assert_eq!(
            classify(StatusCode::NOT_FOUND, String::new()),
            SessionError::Gone(404)
        );
        assert!(classify(StatusCode::SERVICE_UNAVAILABLE, String::new()).is_transient());
        assert!(classify(StatusCode::TOO_MANY_REQUESTS, String::new()).is_transient());
        assert!(matches!(
            classify(StatusCode::FORBIDDEN, "SignatureDoesNotMatch".into()),
            SessionError::Refused { status: 403, .. }
        ));
    }
}
