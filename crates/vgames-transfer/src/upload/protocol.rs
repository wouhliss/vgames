//! GCS resumable requests. Session URLs and signed headers never enter errors.

use std::time::Duration;

use bytes::Bytes;
use reqwest::header::{
    CONTENT_LENGTH, CONTENT_RANGE, HeaderMap, HeaderName, HeaderValue, LOCATION, RANGE,
};
use reqwest::{Client, RequestBuilder, Response, StatusCode, Url};
use tokio_util::sync::CancellationToken;
use vgames_proto::versions::{UploadMethod, UploadTarget};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Confirmed {
    pub offset: u64,
    pub complete: bool,
}

/// Deliberately excludes transport sources, URLs, response bodies and headers.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("upload cancelled")]
    Cancelled,
    #[error("upload request failed temporarily")]
    Retryable,
    #[error("upload session expired")]
    Expired,
    #[error("upload request rejected with status {status}")]
    Rejected { status: u16 },
    #[error("invalid upload protocol: {0}")]
    Invalid(&'static str),
}

impl ProtocolError {
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Retryable)
    }
}

/// `client` must come from `http::transfer_client`, which disables redirects.
pub(super) async fn start(
    client: &Client,
    target: &UploadTarget,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<String, ProtocolError> {
    if target.method != UploadMethod::Post {
        return Err(ProtocolError::Invalid("session start requires POST"));
    }
    let url = validate_url(&target.url)?;
    let headers = target_headers(target)?;
    if headers
        .get("x-goog-resumable")
        .and_then(|v| v.to_str().ok())
        != Some("start")
    {
        return Err(ProtocolError::Invalid("missing resumable start header"));
    }
    let response = send(
        client.post(url).headers(headers).body(Bytes::new()),
        timeout,
        cancel,
    )
    .await?;
    match response.status() {
        StatusCode::OK | StatusCode::CREATED => {}
        status => return Err(rejection(status, false)),
    }
    let mut locations = response.headers().get_all(LOCATION).iter();
    let location = locations
        .next()
        .and_then(|value| value.to_str().ok())
        .ok_or(ProtocolError::Invalid("missing session location"))?;
    if locations.next().is_some() {
        return Err(ProtocolError::Invalid("multiple session locations"));
    }
    validate_url(location)?;
    Ok(location.to_owned())
}

pub(super) async fn status(
    client: &Client,
    session: &str,
    total: u64,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Confirmed, ProtocolError> {
    let request = client
        .put(validate_url(session)?)
        .header(CONTENT_RANGE, "bytes */*")
        .header(CONTENT_LENGTH, 0)
        .body(Bytes::new());
    confirm(send(request, timeout, cancel).await?, total, None)
}

pub(super) async fn put(
    client: &Client,
    session: &str,
    offset: u64,
    total: u64,
    body: Bytes,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Confirmed, ProtocolError> {
    let length =
        u64::try_from(body.len()).map_err(|_| ProtocolError::Invalid("piece length overflow"))?;
    let end = offset
        .checked_add(length)
        .filter(|&end| length > 0 && end <= total)
        .ok_or(ProtocolError::Invalid("piece outside pack"))?;
    let request = client
        .put(validate_url(session)?)
        .header(CONTENT_RANGE, format!("bytes {offset}-{}/{total}", end - 1))
        .header(CONTENT_LENGTH, length)
        .body(body);
    confirm(
        send(request, timeout, cancel).await?,
        total,
        Some((offset, end)),
    )
}

/// Plain signed manifest PUT; uses the same secret-free request boundary.
pub(super) async fn put_manifest(
    client: &Client,
    target: &UploadTarget,
    body: Bytes,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<(), ProtocolError> {
    if target.method != UploadMethod::Put {
        return Err(ProtocolError::Invalid("manifest upload requires PUT"));
    }
    let request = client
        .put(validate_url(&target.url)?)
        .headers(target_headers(target)?)
        .body(body);
    match send(request, timeout, cancel).await?.status() {
        StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => Ok(()),
        status => Err(rejection(status, false)),
    }
}

fn validate_url(value: &str) -> Result<Url, ProtocolError> {
    let invalid = || ProtocolError::Invalid("invalid storage URL");
    if value.trim() != value || value.bytes().any(|b| b.is_ascii_control()) {
        return Err(invalid());
    }
    let url = Url::parse(value).map_err(|_| invalid())?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    let debug_loopback = cfg!(debug_assertions)
        && url.scheme() == "http"
        && url.host_str().is_some_and(|host| {
            host == "localhost"
                || host == "[::1]"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
    if url.scheme() != "https" && !debug_loopback {
        return Err(invalid());
    }
    Ok(url)
}

fn target_headers(target: &UploadTarget) -> Result<HeaderMap, ProtocolError> {
    let mut headers = HeaderMap::new();
    for (name, value) in &target.headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| ProtocolError::Invalid("invalid signed header name"))?;
        let mut value = HeaderValue::from_str(value)
            .map_err(|_| ProtocolError::Invalid("invalid signed header value"))?;
        value.set_sensitive(true);
        if headers.insert(name, value).is_some() {
            return Err(ProtocolError::Invalid("duplicate signed header"));
        }
    }
    Ok(headers)
}

async fn send(
    request: RequestBuilder,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Response, ProtocolError> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(ProtocolError::Cancelled),
        response = tokio::time::timeout(timeout, request.send()) => match response {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(error)) if error.is_builder() => Err(ProtocolError::Invalid("invalid upload request")),
            Ok(Err(_)) | Err(_) => Err(ProtocolError::Retryable),
        }
    }
}

fn rejection(status: StatusCode, session: bool) -> ProtocolError {
    if session && matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE) {
        ProtocolError::Expired
    } else if status.is_server_error()
        || matches!(
            status,
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
        )
    {
        ProtocolError::Retryable
    } else {
        ProtocolError::Rejected {
            status: status.as_u16(),
        }
    }
}

fn confirm(
    response: Response,
    total: u64,
    sent: Option<(u64, u64)>,
) -> Result<Confirmed, ProtocolError> {
    match response.status() {
        StatusCode::OK | StatusCode::CREATED => {
            if sent.is_some_and(|(_, end)| end != total) {
                return Err(ProtocolError::Invalid("completed before the final piece"));
            }
            Ok(Confirmed {
                offset: total,
                complete: true,
            })
        }
        StatusCode::PERMANENT_REDIRECT => {
            if response.headers().contains_key(LOCATION) {
                return Err(ProtocolError::Rejected { status: 308 });
            }
            let offset = confirmed_offset(response.headers(), total)?;
            if sent.is_some_and(|(start, end)| offset < start || offset > end) {
                return Err(ProtocolError::Invalid("acknowledgement outside sent bytes"));
            }
            Ok(Confirmed {
                offset,
                complete: false,
            })
        }
        status => Err(rejection(status, true)),
    }
}

fn confirmed_offset(headers: &HeaderMap, total: u64) -> Result<u64, ProtocolError> {
    let invalid = || ProtocolError::Invalid("invalid acknowledgement range");
    let mut ranges = headers.get_all(RANGE).iter();
    let Some(value) = ranges.next() else {
        return Ok(0);
    };
    if ranges.next().is_some() {
        return Err(invalid());
    }
    let end = value
        .to_str()
        .ok()
        .and_then(|value| value.strip_prefix("bytes=0-"))
        .filter(|end| !end.is_empty() && end.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|end| end.parse::<u64>().ok())
        .ok_or_else(invalid)?;
    end.checked_add(1)
        .filter(|&offset| offset <= total)
        .ok_or_else(invalid)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_bytes, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TIMEOUT: Duration = Duration::from_secs(2);

    fn client() -> Client {
        crate::http::transfer_client(&crate::http::ClientOptions {
            use_system_proxy: false,
            ..Default::default()
        })
        .unwrap()
    }

    fn target(url: String) -> UploadTarget {
        UploadTarget {
            url,
            method: UploadMethod::Post,
            headers: [
                ("Content-Type".into(), "application/octet-stream".into()),
                ("x-goog-resumable".into(), "start".into()),
                ("x-goog-content-length-range".into(), "1,268435456".into()),
            ]
            .into(),
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::minutes(15),
        }
    }

    #[test]
    fn range_is_strict_and_bounded() {
        let mut headers = HeaderMap::new();
        assert_eq!(confirmed_offset(&headers, 100).unwrap(), 0);
        for range in [
            "",
            "bytes=1-9",
            "bytes=0-+9",
            "bytes=0--1",
            "bytes=0-9,10-19",
            "bytes 0-9",
            "bytes=0-9 ",
            "bytes=0-100",
            "bytes=0-18446744073709551615",
        ] {
            headers.insert(RANGE, HeaderValue::from_str(range).unwrap());
            assert!(confirmed_offset(&headers, 100).is_err(), "{range}");
        }
        headers.insert(RANGE, HeaderValue::from_static("bytes=0-49"));
        assert_eq!(confirmed_offset(&headers, 100).unwrap(), 50);
        headers.append(RANGE, HeaderValue::from_static("bytes=0-59"));
        assert!(confirmed_offset(&headers, 100).is_err());
    }

    #[test]
    fn urls_reject_plaintext_non_loopback_credentials_and_fragments() {
        for value in [
            "http://storage.example/session",
            "ftp://storage.example/session",
            "https://name:secret@storage.example/session",
            "https://storage.example/session#secret",
            "https:///",
            " https://storage.example/session",
            "https://storage.example/\nsession",
        ] {
            assert!(validate_url(value).is_err());
        }
        assert!(validate_url("https://storage.example/session").is_ok());
        assert_eq!(
            validate_url("http://127.0.0.1/session").is_ok(),
            cfg!(debug_assertions)
        );
        assert_eq!(
            validate_url("http://[::1]/session").is_ok(),
            cfg!(debug_assertions)
        );
    }

    #[tokio::test]
    async fn start_preserves_signed_headers_and_returns_location() {
        let server = MockServer::start().await;
        let session = format!("{}/session", server.uri());
        Mock::given(method("POST"))
            .and(path("/start"))
            .respond_with(ResponseTemplate::new(201).insert_header("location", session.as_str()))
            .expect(1)
            .mount(&server)
            .await;
        let result = start(
            &client(),
            &target(format!("{}/start", server.uri())),
            TIMEOUT,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result.unwrap(), session);
        let requests = server.received_requests().await.unwrap();
        let sent = requests.first().unwrap();
        assert_eq!(
            sent.headers.get("content-type").unwrap(),
            "application/octet-stream"
        );
        assert_eq!(sent.headers.get("x-goog-resumable").unwrap(), "start");
        assert_eq!(
            sent.headers.get("x-goog-content-length-range").unwrap(),
            "1,268435456"
        );
        assert!(sent.body.is_empty());
    }

    #[tokio::test]
    async fn probes_report_partial_empty_and_complete_sessions() {
        let server = MockServer::start().await;
        for (path_value, response, expected) in [
            (
                "/partial",
                ResponseTemplate::new(308).insert_header("range", "bytes=0-6"),
                Confirmed {
                    offset: 7,
                    complete: false,
                },
            ),
            (
                "/empty",
                ResponseTemplate::new(308),
                Confirmed {
                    offset: 0,
                    complete: false,
                },
            ),
            (
                "/complete",
                ResponseTemplate::new(200),
                Confirmed {
                    offset: 10,
                    complete: true,
                },
            ),
        ] {
            Mock::given(method("PUT"))
                .and(path(path_value))
                .and(header("content-range", "bytes */*"))
                .and(header("content-length", "0"))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
            assert_eq!(
                status(
                    &client(),
                    &format!("{}{path_value}", server.uri()),
                    10,
                    TIMEOUT,
                    &CancellationToken::new()
                )
                .await
                .unwrap(),
                expected
            );
        }
    }

    #[tokio::test]
    async fn pieces_enforce_sent_bounds_and_final_completion() {
        let server = MockServer::start().await;
        for (path_value, response, expected) in [
            (
                "/partial",
                ResponseTemplate::new(308).insert_header("range", "bytes=0-5"),
                Some(6),
            ),
            (
                "/beyond",
                ResponseTemplate::new(308).insert_header("range", "bytes=0-8"),
                None,
            ),
            (
                "/regress",
                ResponseTemplate::new(308).insert_header("range", "bytes=0-2"),
                None,
            ),
            ("/premature", ResponseTemplate::new(201), None),
        ] {
            Mock::given(method("PUT"))
                .and(path(path_value))
                .and(header("content-range", "bytes 4-7/10"))
                .and(body_bytes(b"data".to_vec()))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
            let result = put(
                &client(),
                &format!("{}{path_value}", server.uri()),
                4,
                10,
                Bytes::from_static(b"data"),
                TIMEOUT,
                &CancellationToken::new(),
            )
            .await;
            match expected {
                Some(offset) => assert_eq!(
                    result.unwrap(),
                    Confirmed {
                        offset,
                        complete: false
                    }
                ),
                None => assert!(matches!(result, Err(ProtocolError::Invalid(_)))),
            }
        }
        Mock::given(method("PUT"))
            .and(path("/final"))
            .and(header("content-range", "bytes 8-9/10"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            put(
                &client(),
                &format!("{}/final", server.uri()),
                8,
                10,
                Bytes::from_static(b"ok"),
                TIMEOUT,
                &CancellationToken::new()
            )
            .await
            .unwrap(),
            Confirmed {
                offset: 10,
                complete: true
            }
        );
    }

    #[tokio::test]
    async fn redirects_are_never_followed() {
        let server = MockServer::start().await;
        Mock::given(path("/redirect"))
            .respond_with(
                ResponseTemplate::new(307)
                    .insert_header("location", format!("{}/destination", server.uri())),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/destination"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        assert!(matches!(
            status(
                &client(),
                &format!("{}/redirect", server.uri()),
                10,
                TIMEOUT,
                &CancellationToken::new()
            )
            .await,
            Err(ProtocolError::Rejected { status: 307 })
        ));
    }

    #[tokio::test]
    async fn cancellation_timeouts_and_expired_sessions_are_typed() {
        let server = MockServer::start().await;
        for code in [404, 410, 429, 503] {
            Mock::given(path(format!("/{code}")))
                .respond_with(ResponseTemplate::new(code))
                .mount(&server)
                .await;
            let error = status(
                &client(),
                &format!("{}/{code}", server.uri()),
                10,
                TIMEOUT,
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
            assert_eq!(error.retryable(), code >= 429);
            if code < 429 {
                assert!(matches!(error, ProtocolError::Expired));
            }
        }
        Mock::given(path("/slow"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(100)))
            .mount(&server)
            .await;
        assert!(
            status(
                &client(),
                &format!("{}/slow", server.uri()),
                10,
                Duration::from_millis(5),
                &CancellationToken::new()
            )
            .await
            .unwrap_err()
            .retryable()
        );
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            status(
                &client(),
                &format!("{}/cancelled", server.uri()),
                10,
                TIMEOUT,
                &cancel
            )
            .await,
            Err(ProtocolError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn diagnostics_never_contain_url_or_header_values() {
        let secret_marker = "private-session-marker";
        let mut target = target(format!("https://{secret_marker}@storage.example/session"));
        let error = start(&client(), &target, TIMEOUT, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(!format!("{error:?} {error}").contains(secret_marker));
        target.url = "https://storage.example/start".into();
        target
            .headers
            .insert("x-secret".into(), format!("{secret_marker}\n"));
        let error = start(&client(), &target, TIMEOUT, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(!format!("{error:?} {error}").contains(secret_marker));
        let server = MockServer::start().await;
        Mock::given(path("/session"))
            .respond_with(ResponseTemplate::new(308).insert_header("range", secret_marker))
            .mount(&server)
            .await;
        let error = status(
            &client(),
            &format!("{}/session?opaque={secret_marker}", server.uri()),
            10,
            TIMEOUT,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(!format!("{error:?} {error}").contains(secret_marker));
    }

    #[tokio::test]
    async fn plain_manifest_put_preserves_headers_and_body() {
        let server = MockServer::start().await;
        let mut target = target(format!("{}/manifest", server.uri()));
        target.method = UploadMethod::Put;
        target.headers.remove("x-goog-resumable");
        Mock::given(method("PUT"))
            .and(path("/manifest"))
            .and(header("content-type", "application/octet-stream"))
            .and(body_bytes(b"manifest".to_vec()))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        put_manifest(
            &client(),
            &target,
            Bytes::from_static(b"manifest"),
            TIMEOUT,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    }
}
