//! REST calls to the active server's social endpoints (03-api, `social` tag), and the
//! mapping of problem responses to [`SocialError`].
//!
//! Every call carries the session's bearer token and has a timeout. A `401` reports the
//! token to the session owner ([`SessionSlot::report_unauthorized`]) and returns
//! `not_signed_in`; the call is not retried here.

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
    FriendRequestByCode, FriendRequestByUser, FriendRequestCreate, InboxAck, InboxPage,
    OneTimeKeysStored, OneTimeKeysUpload, PresenceUpdate, SendMessageRequest, SendMessageResponse,
};

use super::model::{SocialError, SocialLimit};
use super::ports::{ServerSession, SessionSlot};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// The HTTP client social calls share (connection pooling across calls).
pub fn http_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("vgames-launcher/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// One call context: a client and the session it runs as.
pub struct SocialApi<'a> {
    pub http: &'a reqwest::Client,
    pub session: ServerSession,
    pub sessions: &'a SessionSlot,
}

#[derive(serde::Deserialize, Default)]
struct Problem {
    #[serde(default)]
    code: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    errors: Vec<ProblemField>,
}

#[derive(serde::Deserialize)]
struct ProblemField {
    field: String,
    #[serde(default)]
    message: Option<String>,
}

/// Maps a problem response to the UI error (05-social-notes §5).
fn map_problem(status: StatusCode, retry_after: Option<u32>, problem: Problem) -> SocialError {
    match (status, problem.code.as_str()) {
        (StatusCode::UNAUTHORIZED, _) => SocialError::NotSignedIn,
        (StatusCode::NOT_FOUND, "friend_code_invalid") => SocialError::CodeInvalid,
        (StatusCode::NOT_FOUND, _) => SocialError::NotFound,
        (StatusCode::TOO_MANY_REQUESTS, _) => SocialError::RateLimited {
            retry_after_seconds: retry_after.unwrap_or(60),
        },
        (StatusCode::CONFLICT, "friend_limit_reached") => SocialError::LimitReached {
            limit: SocialLimit::Friends,
        },
        (StatusCode::CONFLICT, "pending_limit_reached") => SocialError::LimitReached {
            limit: SocialLimit::PendingRequests,
        },
        (StatusCode::CONFLICT, _) => SocialError::Conflict {
            code: problem.code,
            message: problem.title,
        },
        (StatusCode::BAD_REQUEST, _) => match problem.errors.into_iter().next() {
            Some(f) => SocialError::InvalidInput {
                message: f.message.unwrap_or_else(|| problem.title.clone()),
                field: f.field,
            },
            None => SocialError::InvalidInput {
                field: String::new(),
                message: problem.title,
            },
        },
        (s, _) if s.is_server_error() => SocialError::Offline,
        _ => SocialError::Server {
            code: if problem.code.is_empty() {
                status.as_u16().to_string()
            } else {
                problem.code
            },
            message: problem.title,
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
            let mut req = self
                .http
                .request(method.clone(), url.clone())
                .bearer_auth(session.access_token.as_str());
            if let Some(b) = body {
                req = req.json(b);
            }
            let resp = req.send().await.map_err(|e| {
                tracing::debug!(error = %e.without_url(), "social request failed");
                SocialError::Offline
            })?;
            let status = resp.status();
            if status.is_success() {
                return Ok(resp);
            }
            if status == StatusCode::UNAUTHORIZED {
                // Once: the token may just have expired (the session bridge refreshes it).
                if !retried && let Some(fresh) = self.sessions.refreshed(&session).await {
                    retried = true;
                    session = fresh;
                    continue;
                }
                return Err(SocialError::NotSignedIn);
            }
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok());
            let problem = resp.json::<Problem>().await.unwrap_or_default();
            return Err(map_problem(status, retry_after, problem));
        }
    }

    async fn json<T: DeserializeOwned, B: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, SocialError> {
        self.send(method, path, body)
            .await?
            .json::<T>()
            .await
            .map_err(|e| SocialError::internal("reading a server response", &e.without_url()))
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problem(code: &str, field: Option<&str>) -> Problem {
        Problem {
            code: code.into(),
            title: "t".into(),
            errors: field
                .map(|f| {
                    vec![ProblemField {
                        field: f.into(),
                        message: Some("m".into()),
                    }]
                })
                .unwrap_or_default(),
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
                map_problem(status, None, problem(code, field)),
                want,
                "{status} {code}"
            );
        }
        assert_eq!(
            map_problem(
                StatusCode::TOO_MANY_REQUESTS,
                Some(42),
                problem("rate_limited", None)
            ),
            SocialError::RateLimited {
                retry_after_seconds: 42
            }
        );
    }
}
