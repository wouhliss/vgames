//! The launcher's only API client (A2-T07).
//!
//! - rustls with the platform certificate verifier (reqwest's `rustls`
//!   feature), connect and whole-request timeouts, no redirects (a bearer token
//!   never follows one), bodies read with a size cap.
//! - Retries (network errors, 429 with a short `Retry-After`, 502/503/504) for
//!   idempotent requests only.
//! - RFC 9457 problem documents become [`ApiError::Problem`] with the stable
//!   `code`; the UI gets them through the command error types.
//! - Authenticated calls go through [`session::Session`]: a 401 triggers one
//!   single-flight refresh and the request is retried once.

pub mod session;

use std::sync::Arc;
use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

pub use session::Session;

/// Connect timeout for every API request.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Whole-request timeout for ordinary API calls.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest JSON body accepted from the API.
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
/// Retries after the first attempt, idempotent requests only.
const MAX_RETRIES: u32 = 2;
/// Longest `Retry-After` honored automatically; longer waits are left to the user.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(10);

/// Why an API call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    #[error("the server did not answer in time")]
    Timeout,
    #[error("secure connection failed: {0}")]
    Tls(String),
    #[error("the server could not be reached: {0}")]
    Network(String),
    /// A problem document (or a bare error status) from the server.
    #[error("{message} ({code})")]
    Problem {
        status: u16,
        code: String,
        message: String,
        /// The parsed body, for callers that need more than the code (field errors, `Retry-After`).
        problem: Box<ProblemBody>,
    },
    /// No session, or the session ended (refresh refused).
    #[error("sign-in required")]
    Unauthenticated,
    /// The server presented a root key other than the pinned one (01-security §3.1).
    #[error("the server's identity changed; it is blocked")]
    TrustBlocked,
    #[error("the server sent an invalid response: {0}")]
    InvalidResponse(String),
}

/// The parts of an RFC 9457 problem document a caller may act on, bounded and shortened.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProblemBody {
    pub title: Option<String>,
    pub detail: Option<String>,
    /// `validation_failed` field errors (at most [`MAX_PROBLEM_FIELDS`]).
    pub errors: Vec<ProblemField>,
    /// The `Retry-After` header, in seconds, when the server sent one.
    pub retry_after_seconds: Option<u32>,
}

/// One invalid field of a `validation_failed` problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProblemField {
    pub field: String,
    pub message: Option<String>,
}

/// Field errors kept from one problem document.
pub const MAX_PROBLEM_FIELDS: usize = 20;

impl ApiError {
    pub fn problem_code(&self) -> Option<&str> {
        match self {
            Self::Problem { code, .. } => Some(code),
            _ => None,
        }
    }

    /// The parsed problem document, for an error response.
    pub fn problem(&self) -> Option<&ProblemBody> {
        match self {
            Self::Problem { problem, .. } => Some(problem),
            _ => None,
        }
    }

    /// The HTTP status of an error response.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Problem { status, .. } => Some(*status),
            _ => None,
        }
    }

    fn is_retryable(&self) -> bool {
        matches!(self, Self::Timeout | Self::Network(_))
    }

    pub(crate) fn from_reqwest(error: &reqwest::Error) -> Self {
        if error.is_timeout() {
            return Self::Timeout;
        }
        let chain = crate::error::DisplayChain(error).to_string();
        let lower = chain.to_ascii_lowercase();
        if lower.contains("certificate") || lower.contains("tls") || lower.contains("handshake") {
            Self::Tls(short(&chain))
        } else if error.is_decode() || error.is_body() {
            Self::InvalidResponse(short(&chain))
        } else {
            Self::Network(short(&chain))
        }
    }
}

/// Keeps error details readable (and bounded) in the UI.
fn short(text: &str) -> String {
    const MAX: usize = 200;
    match text.char_indices().nth(MAX) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.to_owned(),
    }
}

/// The shared HTTP client for API calls (discovery, auth, catalog, …).
pub fn http_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("vgames-launcher/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Reads at most `cap` bytes of a response body.
pub async fn read_capped(mut response: reqwest::Response, cap: usize) -> Result<Vec<u8>, ApiError> {
    if response
        .content_length()
        .is_some_and(|len| len > cap as u64)
    {
        return Err(ApiError::InvalidResponse("response is too large".into()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| ApiError::from_reqwest(&e))?
    {
        if body.len() + chunk.len() > cap {
            return Err(ApiError::InvalidResponse("response is too large".into()));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Turns an error response into [`ApiError::Problem`].
pub async fn problem_from(response: reqwest::Response) -> ApiError {
    let status = response.status();
    let retry_after = retry_after(&response).and_then(|d| u32::try_from(d.as_secs()).ok());
    let body = read_capped(response, 64 * 1024).await.unwrap_or_default();
    let mut error = parse_problem(status, &body);
    if let ApiError::Problem { problem, .. } = &mut error {
        problem.retry_after_seconds = retry_after;
    }
    error
}

fn parse_problem(status: StatusCode, body: &[u8]) -> ApiError {
    #[derive(serde::Deserialize)]
    struct Field {
        field: String,
        #[serde(default)]
        message: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Body {
        code: Option<String>,
        title: Option<String>,
        detail: Option<String>,
        #[serde(default)]
        errors: Vec<Field>,
    }
    let parsed = serde_json::from_slice::<Body>(body).ok();
    let problem = Box::new(match &parsed {
        Some(b) => ProblemBody {
            title: b.title.as_deref().map(short),
            detail: b.detail.as_deref().map(short),
            errors: b
                .errors
                .iter()
                .take(MAX_PROBLEM_FIELDS)
                .map(|f| ProblemField {
                    field: short(&f.field),
                    message: f.message.as_deref().map(short),
                })
                .collect(),
            retry_after_seconds: None,
        },
        None => ProblemBody::default(),
    });
    let code = parsed
        .as_ref()
        .and_then(|b| b.code.clone())
        .filter(|c| c.len() <= 64 && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
        .unwrap_or_else(|| match status {
            StatusCode::UNAUTHORIZED => "unauthenticated".into(),
            StatusCode::FORBIDDEN => "forbidden".into(),
            StatusCode::NOT_FOUND => "not_found".into(),
            StatusCode::TOO_MANY_REQUESTS => "rate_limited".into(),
            s if s.is_server_error() => "unavailable".into(),
            _ => "http_error".into(),
        });
    let message = parsed
        .and_then(|b| b.detail.or(b.title))
        .map(|m| short(&m))
        .unwrap_or_else(|| {
            status
                .canonical_reason()
                .unwrap_or("Request failed")
                .to_owned()
        });
    ApiError::Problem {
        status: status.as_u16(),
        code,
        message,
        problem,
    }
}

fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    let value = response.headers().get(reqwest::header::RETRY_AFTER)?;
    let seconds: u64 = value.to_str().ok()?.trim().parse().ok()?;
    Some(Duration::from_secs(seconds))
}

fn backoff(attempt: u32) -> Duration {
    let base = 250u64.saturating_mul(1 << attempt.min(6));
    let mut jitter = [0u8; 2];
    let jitter = match getrandom::fill(&mut jitter) {
        Ok(()) => u64::from(u16::from_le_bytes(jitter)) % base.max(1),
        Err(_) => 0,
    };
    Duration::from_millis(base / 2 + jitter / 2)
}

fn is_idempotent(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::HEAD | Method::PUT | Method::DELETE | Method::OPTIONS
    )
}

/// Sends a request built by `build`, retrying idempotent ones. Returns the
/// final response whatever its status (the caller maps error statuses).
pub async fn send_with_retries<F>(method: &Method, build: F) -> Result<reqwest::Response, ApiError>
where
    F: Fn() -> reqwest::RequestBuilder,
{
    let retries = if is_idempotent(method) {
        MAX_RETRIES
    } else {
        0
    };
    let mut attempt = 0;
    loop {
        let result = build().send().await;
        let wait = match &result {
            Ok(response) => match response.status() {
                StatusCode::TOO_MANY_REQUESTS => {
                    retry_after(response).filter(|d| *d <= MAX_RETRY_AFTER)
                }
                StatusCode::BAD_GATEWAY
                | StatusCode::SERVICE_UNAVAILABLE
                | StatusCode::GATEWAY_TIMEOUT => {
                    Some(retry_after(response).unwrap_or_else(|| backoff(attempt)))
                        .filter(|d| *d <= MAX_RETRY_AFTER)
                }
                _ => None,
            },
            Err(error) => ApiError::from_reqwest(error)
                .is_retryable()
                .then(|| backoff(attempt)),
        };
        match (wait, attempt < retries) {
            (Some(wait), true) => {
                attempt += 1;
                tracing::debug!(
                    attempt,
                    wait_ms = wait.as_millis() as u64,
                    "retrying API request"
                );
                tokio::time::sleep(wait).await;
            }
            _ => return result.map_err(|e| ApiError::from_reqwest(&e)),
        }
    }
}

/// Decodes a successful JSON response, or maps the error status.
pub async fn json_or_problem<T: DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, ApiError> {
    if !response.status().is_success() {
        return Err(problem_from(response).await);
    }
    let body = read_capped(response, MAX_BODY_BYTES).await?;
    serde_json::from_slice(&body).map_err(|e| ApiError::InvalidResponse(short(&e.to_string())))
}

/// An API client bound to one server. Cheap to clone.
#[derive(Clone)]
pub struct ApiClient {
    http: reqwest::Client,
    base: Url,
    session: Arc<Session>,
}

impl ApiClient {
    pub fn new(http: reqwest::Client, base: Url, session: Arc<Session>) -> Self {
        Self {
            http,
            base,
            session,
        }
    }

    pub fn base(&self) -> &Url {
        &self.base
    }

    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }

    /// `path` is relative to the server root (`v1/…`).
    pub fn url(&self, path: &str) -> Result<Url, ApiError> {
        self.base
            .join(path)
            .map_err(|e| ApiError::InvalidResponse(e.to_string()))
    }

    /// An unauthenticated request.
    pub async fn public<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, ApiError> {
        self.session.check_trusted()?;
        let url = self.url(path)?;
        let response = send_with_retries(&method, || {
            let request = self.http.request(method.clone(), url.clone());
            match body {
                Some(body) => request.json(body),
                None => request,
            }
        })
        .await?;
        json_or_problem(response).await
    }

    /// An authenticated request: bearer token, one refresh + one retry on 401.
    pub async fn authed<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, ApiError> {
        let response = self.authed_response(method, path, body).await?;
        json_or_problem(response).await
    }

    /// Like [`Self::authed`] for endpoints answering `204 No Content`.
    pub async fn authed_empty<B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<(), ApiError> {
        let response = self.authed_response(method, path, body).await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(problem_from(response).await)
        }
    }

    /// An authenticated `GET` answered by a redirect (`/v1/assets/{id}`):
    /// returns the target without following it, so the bearer token never
    /// leaves this server.
    pub async fn authed_location(&self, path: &str) -> Result<Url, ApiError> {
        let response = self.authed_response::<()>(Method::GET, path, None).await?;
        if !response.status().is_redirection() {
            return Err(if response.status().is_success() {
                ApiError::InvalidResponse("expected a redirect".into())
            } else {
                problem_from(response).await
            });
        }
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ApiError::InvalidResponse("redirect without a location".into()))?;
        response
            .url()
            .join(location)
            .map_err(|_| ApiError::InvalidResponse("invalid redirect location".into()))
    }

    async fn authed_response<B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<reqwest::Response, ApiError> {
        let url = self.url(path)?;
        self.with_auth(&method, |http, token| {
            let request = http.request(method.clone(), url.clone()).bearer_auth(token);
            match body {
                Some(body) => request.json(body),
                None => request,
            }
        })
        .await
    }

    /// The "401 → refresh once → retry once" hook for any authenticated request. `build` gets the HTTP
    /// client and the current access token and returns the request (URL, query, body, bearer header).
    /// Idempotent methods also get the usual network retries. Returns the final response whatever its
    /// status, except that a request still refused after one refresh becomes
    /// [`ApiError::Unauthenticated`]; map other statuses with [`problem_from`].
    pub async fn with_auth<F>(
        &self,
        method: &Method,
        build: F,
    ) -> Result<reqwest::Response, ApiError>
    where
        F: Fn(&reqwest::Client, &str) -> reqwest::RequestBuilder,
    {
        self.session.check_trusted()?;
        let grant = self.session.access_token(&self.http).await?;
        let response = send_with_retries(method, || build(&self.http, &grant.token)).await?;
        if response.status() != StatusCode::UNAUTHORIZED {
            return Ok(response);
        }
        // Once: refresh (shared with concurrent callers), then retry.
        let grant = self.session.refresh(&self.http, grant.generation).await?;
        let response = send_with_retries(method, || build(&self.http, &grant.token)).await?;
        if response.status() == StatusCode::UNAUTHORIZED {
            let error = problem_from(response).await;
            tracing::info!(%error, "request still unauthorized after a refresh");
            return Err(ApiError::Unauthenticated);
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problems_keep_the_stable_code() {
        let body = br#"{"type":"urn:vgames:problem:rate_limited","title":"Slow down","status":429,"code":"rate_limited","detail":"Try again in 5 s"}"#;
        assert_eq!(
            parse_problem(StatusCode::TOO_MANY_REQUESTS, body),
            ApiError::Problem {
                status: 429,
                code: "rate_limited".into(),
                message: "Try again in 5 s".into(),
                problem: Box::new(ProblemBody {
                    title: Some("Slow down".into()),
                    detail: Some("Try again in 5 s".into()),
                    errors: vec![],
                    retry_after_seconds: None,
                }),
            }
        );
    }

    #[test]
    fn field_errors_are_kept_bounded() {
        let errors: Vec<_> = (0..50)
            .map(|i| serde_json::json!({ "field": format!("files[{i}].path"), "message": "bad" }))
            .collect();
        let body = serde_json::to_vec(&serde_json::json!({
            "title": "Invalid", "status": 400, "code": "validation_failed", "errors": errors
        }))
        .unwrap();
        let error = parse_problem(StatusCode::BAD_REQUEST, &body);
        let problem = error.problem().unwrap();
        assert_eq!(problem.errors.len(), MAX_PROBLEM_FIELDS);
        assert_eq!(problem.errors[3].field, "files[3].path");
        assert_eq!(problem.errors[3].message.as_deref(), Some("bad"));
        assert_eq!(error.status(), Some(400));
    }

    #[test]
    fn bare_statuses_and_odd_codes_get_a_generic_code() {
        let error = parse_problem(StatusCode::SERVICE_UNAVAILABLE, b"<html>oops</html>");
        assert_eq!(error.problem_code(), Some("unavailable"));
        let error = parse_problem(StatusCode::FORBIDDEN, br#"{"code":"<script>"}"#);
        assert_eq!(error.problem_code(), Some("forbidden"));
    }

    #[test]
    fn only_idempotent_methods_retry() {
        assert!(is_idempotent(&Method::GET));
        assert!(!is_idempotent(&Method::POST));
        assert!(!is_idempotent(&Method::PATCH));
    }

    #[test]
    fn long_details_are_shortened() {
        let text = "é".repeat(500);
        assert_eq!(short(&text).chars().count(), 201);
    }
}
