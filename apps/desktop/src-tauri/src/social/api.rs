//! REST calls to the active server's social endpoints (03-api, `social` tag), and the
//! mapping of problem responses to [`SocialError`].
//!
//! Calls go through the API client's transport: [`crate::api::send_with_retries`] (network
//! retries and short waits for idempotent calls only) and [`crate::api::problem_from`]
//! (the bounded RFC 9457 body, a sanitized `code`, field errors, `Retry-After`). Every call
//! carries the session's bearer token and has a timeout. A `401` asks the session bridge
//! for a fresh token once (the server's single-flight `api::Session::refresh`) and then
//! returns `not_signed_in`.
//!
//! Redirects are never followed (a bearer token must not reach another host), and every
//! body is read through [`crate::api::read_capped`]: [`MAX_PROBLEM_BYTES`] for problems,
//! [`MAX_SUCCESS_BYTES`] for results (the API's own 4 MiB cap is too small for a full inbox page). A body the launcher cannot parse is logged by
//! its error category and position only, never by the server's text.

use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;
use vgames_proto::auth::UserPublic;
use vgames_proto::realtime::RealtimeTicket;
use vgames_proto::social::{
    ClaimKeysRequest, ClaimedKeyList, Conversation, ConversationCreate, ConversationPage, Device,
    DeviceKeysList, DeviceList, DeviceRegister, Friend, FriendCode, FriendList,
    FriendRequestByCode, FriendRequestByUser, FriendRequestCreate, InboxAck, InboxPage, Invite,
    InviteCreate, InviteList, InviteStatusUpdate, OneTimeKeysStored, OneTimeKeysUpload,
    PresenceUpdate, SendMessageRequest, SendMessageResponse,
};

use super::model::{SocialError, SocialLimit};
use super::ports::{ServerSession, SessionSlot};
use crate::api::{ApiError, ProblemBody};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Largest problem body read from an error response.
pub const MAX_PROBLEM_BYTES: usize = crate::api::MAX_PROBLEM_BYTES;
/// Largest successful response body. The biggest legal one is an inbox page
/// (`inbox`, `limit=100`): 100 envelopes of up to `MAX_ENVELOPE_CIPHERTEXT`
/// (64 KiB) of ciphertext, about 87 KiB each in base64 plus metadata, so about
/// 8.6 MiB. 16 MiB keeps that page legal with room for metadata growth.
pub const MAX_SUCCESS_BYTES: usize = 16 * 1024 * 1024;

/// The HTTP client social calls share (connection pooling across calls).
pub fn http_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("vgames-launcher/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// One call context: a client and the session it runs as.
pub struct SocialApi<'a> {
    pub http: &'a reqwest::Client,
    pub session: ServerSession,
    pub sessions: &'a SessionSlot,
}

/// Maps an API failure to the UI error (05-social-notes §5).
fn map_error(error: ApiError) -> SocialError {
    match error {
        ApiError::Timeout | ApiError::Network(_) | ApiError::Tls(_) => SocialError::Offline,
        ApiError::Unauthenticated | ApiError::TrustBlocked => SocialError::NotSignedIn,
        ApiError::InvalidResponse(_) => SocialError::Internal {
            detail: "reading a server response".to_owned(),
        },
        ApiError::Problem {
            status,
            code,
            message,
            problem,
        } => map_problem(
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            code,
            message,
            *problem,
        ),
    }
}

/// Maps a problem response (already bounded and checked by [`crate::api::problem_from`]).
fn map_problem(
    status: StatusCode,
    code: String,
    message: String,
    problem: ProblemBody,
) -> SocialError {
    let title = problem.title.unwrap_or_else(|| message.clone());
    match (status, code.as_str()) {
        (StatusCode::UNAUTHORIZED, _) => SocialError::NotSignedIn,
        (StatusCode::NOT_FOUND, "friend_code_invalid") => SocialError::CodeInvalid,
        (StatusCode::NOT_FOUND, _) => SocialError::NotFound,
        (StatusCode::TOO_MANY_REQUESTS, _) => SocialError::RateLimited {
            retry_after_seconds: problem.retry_after_seconds.unwrap_or(60),
        },
        (StatusCode::CONFLICT, "friend_limit_reached") => SocialError::LimitReached {
            limit: SocialLimit::Friends,
        },
        (StatusCode::CONFLICT, "pending_limit_reached") => SocialError::LimitReached {
            limit: SocialLimit::PendingRequests,
        },
        (StatusCode::CONFLICT, _) => SocialError::Conflict {
            code,
            message: title,
        },
        (StatusCode::BAD_REQUEST, _) => match problem.errors.into_iter().next() {
            Some(f) => SocialError::InvalidInput {
                message: f.message.unwrap_or(title),
                field: f.field,
            },
            None => SocialError::InvalidInput {
                field: String::new(),
                message: title,
            },
        },
        (s, _) if s.is_server_error() => SocialError::Offline,
        // A bare status (no usable server code) is named by its number, as before.
        _ => SocialError::Server {
            code: if code == "http_error" {
                status.as_u16().to_string()
            } else {
                code
            },
            message: title,
        },
    }
}

impl SocialApi<'_> {
    fn url(&self, path: &str) -> Result<url::Url, SocialError> {
        self.session
            .base_url
            .join(path.trim_start_matches('/'))
            .map_err(|e| SocialError::internal("building a server URL", &e))
    }

    async fn send<B: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<reqwest::Response, SocialError> {
        let url = self.url(path)?;
        let mut session = self.session.clone();
        let mut retried = false;
        loop {
            // ApiClient's transport: network retries for idempotent calls, no redirects.
            let resp = crate::api::send_with_retries(&method, || {
                let req = self
                    .http
                    .request(method.clone(), url.clone())
                    .bearer_auth(session.access_token.as_str());
                match body {
                    Some(b) => req.json(b),
                    None => req,
                }
            })
            .await
            .map_err(|e| {
                tracing::debug!(error = %e, "social request failed");
                map_error(e)
            })?;
            let status = resp.status();
            if status.is_success() {
                return Ok(resp);
            }
            if status == StatusCode::UNAUTHORIZED {
                // Once: the bridge refreshes through the server's single-flight `Session::refresh`.
                if !retried && let Some(fresh) = self.sessions.refreshed(&session).await {
                    retried = true;
                    session = fresh;
                    continue;
                }
                return Err(SocialError::NotSignedIn);
            }
            return Err(map_error(crate::api::problem_from(resp).await));
        }
    }

    async fn json<T: DeserializeOwned, B: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, SocialError> {
        let resp = self.send(method, path, body).await?;
        let bytes = crate::api::read_capped(resp, MAX_SUCCESS_BYTES)
            .await
            .map_err(|e| SocialError::internal("reading a server response", &e))?;
        serde_json::from_slice(&bytes).map_err(|e| {
            // serde's message can quote the server's text: log only where it failed.
            tracing::error!(
                category = ?e.classify(),
                line = e.line(),
                column = e.column(),
                bytes = bytes.len(),
                "unreadable social response"
            );
            SocialError::Internal {
                detail: "reading a server response".to_owned(),
            }
        })
    }

    async fn empty<B: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<(), SocialError> {
        self.send(method, path, body).await.map(drop)
    }

    pub async fn friends(&self) -> Result<FriendList, SocialError> {
        self.json(Method::GET, "v1/friends", None::<&()>).await
    }

    pub async fn friend_code(&self) -> Result<FriendCode, SocialError> {
        self.json(Method::POST, "v1/friend-codes", None::<&()>)
            .await
    }

    pub async fn request_by_code(&self, code: &str) -> Result<Friend, SocialError> {
        let body = FriendRequestCreate::Code(FriendRequestByCode {
            friend_code: code.to_owned(),
        });
        self.json(Method::POST, "v1/friends/requests", Some(&body))
            .await
    }

    pub async fn request_by_user(&self, user_id: Uuid) -> Result<Friend, SocialError> {
        let body = FriendRequestCreate::User(FriendRequestByUser { user_id });
        self.json(Method::POST, "v1/friends/requests", Some(&body))
            .await
    }

    pub async fn accept(&self, user_id: Uuid) -> Result<Friend, SocialError> {
        self.json(
            Method::POST,
            &format!("v1/friends/{user_id}/accept"),
            None::<&()>,
        )
        .await
    }

    pub async fn decline(&self, user_id: Uuid) -> Result<(), SocialError> {
        self.empty(
            Method::POST,
            &format!("v1/friends/{user_id}/decline"),
            None::<&()>,
        )
        .await
    }

    pub async fn remove(&self, user_id: Uuid) -> Result<(), SocialError> {
        self.empty(
            Method::DELETE,
            &format!("v1/friends/{user_id}"),
            None::<&()>,
        )
        .await
    }

    pub async fn block(&self, user_id: Uuid) -> Result<(), SocialError> {
        self.empty(Method::POST, &format!("v1/blocks/{user_id}"), None::<&()>)
            .await
    }

    pub async fn unblock(&self, user_id: Uuid) -> Result<(), SocialError> {
        self.empty(Method::DELETE, &format!("v1/blocks/{user_id}"), None::<&()>)
            .await
    }

    pub async fn profile(&self, user_id: Uuid) -> Result<UserPublic, SocialError> {
        self.json(Method::GET, &format!("v1/users/{user_id}"), None::<&()>)
            .await
    }

    pub async fn set_presence(&self, update: &PresenceUpdate) -> Result<(), SocialError> {
        self.empty(Method::PUT, "v1/presence", Some(update)).await
    }

    pub async fn realtime_ticket(&self) -> Result<RealtimeTicket, SocialError> {
        self.json(Method::POST, "v1/realtime/ticket", None::<&()>)
            .await
    }

    // ---- devices and keys (A4-T08) ------------------------------------------------------------

    /// Registers this install's keys and binds the session to the device.
    pub async fn register_device(&self, body: &DeviceRegister) -> Result<Device, SocialError> {
        self.json(Method::POST, "v1/devices", Some(body)).await
    }

    pub async fn my_devices(&self) -> Result<DeviceList, SocialError> {
        self.json(Method::GET, "v1/devices", None::<&()>).await
    }

    pub async fn revoke_device(&self, device_id: Uuid) -> Result<(), SocialError> {
        self.empty(
            Method::DELETE,
            &format!("v1/devices/{device_id}"),
            None::<&()>,
        )
        .await
    }

    pub async fn upload_keys(
        &self,
        device_id: Uuid,
        body: &OneTimeKeysUpload,
    ) -> Result<OneTimeKeysStored, SocialError> {
        self.json(
            Method::POST,
            &format!("v1/devices/{device_id}/one-time-keys"),
            Some(body),
        )
        .await
    }

    pub async fn user_devices(&self, user_id: Uuid) -> Result<DeviceKeysList, SocialError> {
        self.json(
            Method::GET,
            &format!("v1/users/{user_id}/devices"),
            None::<&()>,
        )
        .await
    }

    pub async fn claim_keys(&self, body: &ClaimKeysRequest) -> Result<ClaimedKeyList, SocialError> {
        self.json(Method::POST, "v1/keys/claim", Some(body)).await
    }

    // ---- conversations and the relay (A4-T08) -------------------------------------------------

    pub async fn conversations(
        &self,
        cursor: Option<&str>,
    ) -> Result<ConversationPage, SocialError> {
        let mut path = "v1/conversations?limit=100".to_owned();
        if let Some(c) = cursor {
            path.push_str("&cursor=");
            path.push_str(&url::form_urlencoded::byte_serialize(c.as_bytes()).collect::<String>());
        }
        self.json(Method::GET, &path, None::<&()>).await
    }

    pub async fn create_conversation(
        &self,
        body: &ConversationCreate,
    ) -> Result<Conversation, SocialError> {
        self.json(Method::POST, "v1/conversations", Some(body))
            .await
    }

    pub async fn send_message(
        &self,
        conversation_id: Uuid,
        body: &SendMessageRequest,
    ) -> Result<SendMessageResponse, SocialError> {
        self.json(
            Method::POST,
            &format!("v1/conversations/{conversation_id}/messages"),
            Some(body),
        )
        .await
    }

    /// The oldest undelivered envelopes for this session's device.
    pub async fn inbox(&self) -> Result<InboxPage, SocialError> {
        self.json(Method::GET, "v1/inbox?limit=100", None::<&()>)
            .await
    }

    pub async fn ack(&self, body: &InboxAck) -> Result<(), SocialError> {
        self.empty(Method::POST, "v1/inbox/ack", Some(body)).await
    }

    // ---- invites (A4-T09) ---------------------------------------------------------------------

    /// Active invites and those that ended recently, both directions.
    pub async fn invites(&self) -> Result<InviteList, SocialError> {
        self.json(Method::GET, "v1/invites", None::<&()>).await
    }

    pub async fn invite_create(&self, body: &InviteCreate) -> Result<Invite, SocialError> {
        self.json(Method::POST, "v1/invites", Some(body)).await
    }

    /// `accept`, `decline` or `cancel`.
    pub async fn invite_action(&self, id: Uuid, action: &str) -> Result<Invite, SocialError> {
        self.json(
            Method::POST,
            &format!("v1/invites/{id}/{action}"),
            None::<&()>,
        )
        .await
    }

    pub async fn invite_status(
        &self,
        id: Uuid,
        body: &InviteStatusUpdate,
    ) -> Result<Invite, SocialError> {
        self.json(Method::POST, &format!("v1/invites/{id}/status"), Some(body))
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroize::Zeroizing;

    use super::*;

    const TOKEN: &str = "social-test-token";

    fn session(base: &str) -> ServerSession {
        ServerSession {
            server_id: Uuid::now_v7(),
            user_id: Uuid::now_v7(),
            base_url: url::Url::parse(base).unwrap(),
            access_token: Arc::new(Zeroizing::new(TOKEN.to_owned())),
        }
    }

    #[tokio::test]
    async fn reads_retry_a_brief_outage_and_writes_never_do() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/friends"))
            .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "0"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/friends"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "friends": [], "incoming": [], "outgoing": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/friend-codes"))
            .respond_with(ResponseTemplate::new(503).set_body_json(serde_json::json!({
                "code": "<script>", "title": "down"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let http = http_client().unwrap();
        let sessions = SessionSlot::new();
        let api = SocialApi {
            http: &http,
            session: session(&format!("{}/", server.uri())),
            sessions: &sessions,
        };
        let list = api.friends().await.unwrap();
        assert!(list.friends.is_empty());
        assert_eq!(api.friend_code().await.unwrap_err(), SocialError::Offline);
    }

    #[tokio::test]
    async fn a_server_code_that_is_not_a_plain_identifier_is_not_shown() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/friends"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "code": "<img src=x>", "title": "No"
            })))
            .mount(&server)
            .await;
        let http = http_client().unwrap();
        let sessions = SessionSlot::new();
        let api = SocialApi {
            http: &http,
            session: session(&format!("{}/", server.uri())),
            sessions: &sessions,
        };
        assert_eq!(
            api.friends().await.unwrap_err(),
            SocialError::Server {
                code: "forbidden".into(),
                message: "No".into()
            }
        );
    }

    #[tokio::test]
    async fn a_redirect_is_never_followed_and_the_token_stays_home() {
        let target = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"friends": []})),
            )
            .mount(&target)
            .await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/friends"))
            .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/v1/friends", target.uri()).as_str()),
            )
            .expect(1)
            .mount(&server)
            .await;
        let http = http_client().unwrap();
        let sessions = SessionSlot::new();
        let api = SocialApi {
            http: &http,
            session: session(&format!("{}/", server.uri())),
            sessions: &sessions,
        };
        let err = api.friends().await.unwrap_err();
        assert!(
            matches!(&err, SocialError::Server { code, .. } if code == "302"),
            "{err:?}"
        );
        assert!(
            target
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty(),
            "the redirect target was contacted"
        );
    }

    #[tokio::test]
    async fn a_full_inbox_page_at_the_maximum_ciphertext_size_is_accepted() {
        use base64::Engine as _;
        use vgames_proto::social::{InboxEnvelope, InboxPage, MAX_ENVELOPE_CIPHERTEXT};
        let ciphertext =
            base64::engine::general_purpose::STANDARD.encode(vec![0xa5; MAX_ENVELOPE_CIPHERTEXT]);
        let page = InboxPage {
            items: (0..100u128)
                .map(|i| InboxEnvelope {
                    id: Uuid::from_u128(i),
                    conversation_id: Uuid::from_u128(1000),
                    sender_user_id: Uuid::from_u128(2000),
                    sender_device_id: Uuid::from_u128(3000),
                    sender_identity_key: "k".repeat(43),
                    algorithm: "olm.v1".into(),
                    olm_message_type: 1,
                    ciphertext: ciphertext.clone(),
                    created_at: time::OffsetDateTime::UNIX_EPOCH,
                })
                .collect(),
            next_cursor: Some("c".repeat(64)),
        };
        let body = serde_json::to_vec(&page).unwrap();
        assert!(
            body.len() > 8 * 1024 * 1024 && body.len() < MAX_SUCCESS_BYTES,
            "{} bytes",
            body.len()
        );
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/inbox"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .expect(1)
            .mount(&server)
            .await;
        let http = http_client().unwrap();
        let sessions = SessionSlot::new();
        let api = SocialApi {
            http: &http,
            session: session(&format!("{}/", server.uri())),
            sessions: &sessions,
        };
        assert_eq!(api.inbox().await.unwrap(), page);
    }

    /// Serves one response whose chunked body never ends before 100 MiB; returns the
    /// address and a counter of the body bytes the client accepted.
    async fn endless_body(status: &str) -> (std::net::SocketAddr, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sent = Arc::new(AtomicUsize::new(0));
        let counter = sent.clone();
        let status = status.to_owned();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = vec![0u8; 8192];
            let _ = sock.read(&mut req).await;
            let head = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n"
            );
            if sock.write_all(head.as_bytes()).await.is_err() {
                return;
            }
            let chunk = vec![b' '; 1024 * 1024];
            for _ in 0..100 {
                let mut frame = format!("{:x}\r\n", chunk.len()).into_bytes();
                frame.extend_from_slice(&chunk);
                frame.extend_from_slice(b"\r\n");
                if sock.write_all(&frame).await.is_err() {
                    return;
                }
                counter.fetch_add(chunk.len(), Ordering::SeqCst);
            }
            let _ = sock.write_all(b"0\r\n\r\n").await;
        });
        (addr, sent)
    }

    #[tokio::test]
    async fn a_100_mib_body_is_refused_at_the_cap() {
        for status in ["200 OK", "409 Conflict"] {
            let (addr, sent) = endless_body(status).await;
            let http = http_client().unwrap();
            let sessions = SessionSlot::new();
            let api = SocialApi {
                http: &http,
                session: session(&format!("http://{addr}/")),
                sessions: &sessions,
            };
            let err = api.friends().await.unwrap_err();
            match status {
                "200 OK" => assert!(matches!(err, SocialError::Internal { .. }), "{err:?}"),
                _ => assert!(
                    matches!(&err, SocialError::Conflict { code, .. } if code == "http_error"),
                    "{err:?}"
                ),
            }
            // The client stopped reading: the server could never push the whole 100 MiB.
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(
                sent.load(Ordering::SeqCst) < 100 * 1024 * 1024,
                "{status}: the whole body was read"
            );
        }
    }

    fn problem(field: Option<&str>) -> ProblemBody {
        ProblemBody {
            title: Some("t".into()),
            errors: field
                .map(|f| {
                    vec![crate::api::ProblemField {
                        field: f.into(),
                        message: Some("m".into()),
                    }]
                })
                .unwrap_or_default(),
            ..ProblemBody::default()
        }
    }

    #[test]
    fn problems_map_to_ui_errors() {
        let cases = [
            (
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                None,
                SocialError::NotSignedIn,
            ),
            (
                StatusCode::NOT_FOUND,
                "friend_code_invalid",
                None,
                SocialError::CodeInvalid,
            ),
            (
                StatusCode::NOT_FOUND,
                "not_found",
                None,
                SocialError::NotFound,
            ),
            (
                StatusCode::CONFLICT,
                "friend_limit_reached",
                None,
                SocialError::LimitReached {
                    limit: SocialLimit::Friends,
                },
            ),
            (
                StatusCode::CONFLICT,
                "pending_limit_reached",
                None,
                SocialError::LimitReached {
                    limit: SocialLimit::PendingRequests,
                },
            ),
            (
                StatusCode::CONFLICT,
                "already_friends",
                None,
                SocialError::Conflict {
                    code: "already_friends".into(),
                    message: "t".into(),
                },
            ),
            (
                StatusCode::BAD_REQUEST,
                "validation_failed",
                Some("friend_code"),
                SocialError::invalid("friend_code", "m"),
            ),
            (StatusCode::BAD_GATEWAY, "", None, SocialError::Offline),
            (
                StatusCode::FORBIDDEN,
                "user_disabled",
                None,
                SocialError::Server {
                    code: "user_disabled".into(),
                    message: "t".into(),
                },
            ),
        ];
        for (status, code, field, want) in cases {
            assert_eq!(
                map_problem(status, code.into(), "detail".into(), problem(field)),
                want,
                "{status} {code}"
            );
        }
        assert_eq!(
            map_problem(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited".into(),
                "detail".into(),
                ProblemBody {
                    retry_after_seconds: Some(42),
                    ..problem(None)
                }
            ),
            SocialError::RateLimited {
                retry_after_seconds: 42
            }
        );
    }
}
