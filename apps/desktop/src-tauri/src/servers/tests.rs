//! A2-T07 acceptance tests against a mock server: TOFU pin, fingerprint
//! mismatch block, trust rollback refused, refresh-token rotation, and
//! 401 → one refresh then one retry.

use std::sync::Mutex as StdMutex;

use serde_json::{Value, json};
use tokio::sync::broadcast;
use vgames_core::sign::SecretKey;
use vgames_core::trust::sign_bundle;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::auth::{AuthError, AuthOutcome, pkce_challenge};
use super::*;
use crate::events::AuthFinished;
use crate::secrets::{MemoryVault, TokenVault as _};

const SERVER_ID: &str = "01920000-0000-7000-8000-000000000001";

fn server_id() -> Uuid {
    Uuid::parse_str(SERVER_ID).unwrap()
}

fn key(seed: u8) -> SecretKey {
    SecretKey::from_seed(&[seed; 32])
}

#[derive(Default)]
struct RecordingBrowser {
    opened: StdMutex<Vec<Url>>,
}

impl BrowserOpener for RecordingBrowser {
    fn open(&self, url: &Url) -> bool {
        self.opened.lock().unwrap().push(url.clone());
        true
    }
}

struct Harness {
    servers: Servers,
    mock: MockServer,
    db: Db,
    bus: EventBus,
    events: broadcast::Receiver<AppEvent>,
    vault: Arc<MemoryVault>,
    browser: Arc<RecordingBrowser>,
}

impl Harness {
    async fn new() -> Self {
        let db = Db::open_in_memory().unwrap();
        let mock = MockServer::start().await;
        Self::with(db, mock, Arc::new(MemoryVault::default()))
    }

    fn with(db: Db, mock: MockServer, vault: Arc<MemoryVault>) -> Self {
        let bus = EventBus::new();
        let events = bus.subscribe();
        let browser = Arc::new(RecordingBrowser::default());
        let config = ServersConfig {
            allow_loopback_http: true,
            launcher_version: semver::Version::new(1, 0, 0),
            device_name: "test-pc".into(),
            browser: Arc::clone(&browser) as Arc<dyn BrowserOpener>,
            preview_ttl: PREVIEW_TTL,
        };
        let servers = Servers::new(
            db.clone(),
            bus.clone(),
            crate::api::http_client().unwrap(),
            VaultHandle(Arc::clone(&vault) as Arc<dyn crate::secrets::TokenVault>),
            config,
        );
        Self {
            servers,
            mock,
            db,
            bus,
            events,
            vault,
            browser,
        }
    }

    /// A fresh `Servers` on the same database and vault (a launcher restart).
    fn restart(self) -> Self {
        let Self {
            db, mock, vault, ..
        } = self;
        Self::with(db, mock, vault)
    }

    fn events(&mut self) -> Vec<AppEvent> {
        let mut out = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            out.push(event);
        }
        out
    }

    async fn serve_identity(&self, root: &SecretKey) {
        let key = root.public_key();
        Mock::given(method("GET"))
            .and(path("/.well-known/vgames.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "format": "vgames.server/1",
                "server_id": SERVER_ID,
                "name": "Friends",
                "motd": "Welcome",
                "api_versions": ["v1"],
                "root_public_key": key.to_base64(),
                "root_key_fingerprint": key.fingerprint().to_string(),
                "registration_mode": "allowlist",
                "features": ["social"],
                "min_launcher_version": "0.1.0"
            })))
            .mount(&self.mock)
            .await;
    }

    async fn serve_bundle(&self, body: Value) {
        Mock::given(method("GET"))
            .and(path("/v1/trust/bundle"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&self.mock)
            .await;
    }

    /// Previews and confirms the mock server with `root` (bundle v1 served).
    async fn add(&mut self, root: &SecretKey) -> ServerProfile {
        self.serve_identity(root).await;
        self.serve_bundle(bundle(root, 1, None)).await;
        let preview = self.servers.preview(&self.mock.uri(), None).await.unwrap();
        self.servers.confirm(preview.preview_id).await.unwrap()
    }

    /// Replaces every mock.
    async fn reset(&self) {
        self.mock.reset().await;
    }
}

/// A signed `vgames.trust/1` bundle as served by `GET /v1/trust/bundle`.
fn bundle(signer: &SecretKey, version: u64, next_root: Option<&SecretKey>) -> Value {
    let next_root = next_root.map(|k| {
        let key = k.public_key();
        json!({ "public_key": key.to_base64(), "key_id": key.key_id().to_hex() })
    });
    let bytes = serde_json::to_vec(&json!({
        "format": "vgames.trust/1",
        "server_id": SERVER_ID,
        "version": version,
        "issued_at": "2026-09-24T10:00:00Z",
        "expires_at": null,
        "root_key_id": signer.public_key().key_id().to_hex(),
        "publishers": [],
        "revoked": [],
        "next_root": next_root
    }))
    .unwrap();
    let signature = sign_bundle(signer, &bytes);
    json!({
        "bundle": vgames_core::codec::encode_base64(&bytes),
        "signature": signature.to_base64()
    })
}

fn token_response(access: &str, refresh: &str) -> Value {
    json!({
        "access_token": access,
        "refresh_token": refresh,
        "token_type": "Bearer",
        "expires_in": 900,
        "user": {
            "id": "01920000-0000-7000-8000-00000000000a",
            "username": "alice",
            "display_name": "Alice",
            "role": "admin",
            "created_at": "2026-09-24T10:00:00Z"
        }
    })
}

fn body_json(request: &Request) -> Value {
    serde_json::from_slice(&request.body).unwrap()
}

async fn requests_to(mock: &MockServer, route: &str) -> Vec<Request> {
    mock.received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == route)
        .collect()
}

#[tokio::test]
async fn first_connection_pins_the_root_on_confirmation() {
    let mut h = Harness::new().await;
    let root = key(1);
    h.serve_identity(&root).await;
    h.serve_bundle(bundle(&root, 3, None)).await;

    let preview = h.servers.preview(&h.mock.uri(), None).await.unwrap();
    assert_eq!(
        preview.fingerprint,
        root.public_key().fingerprint().to_string()
    );
    assert_eq!(preview.server_id, server_id());
    assert_eq!(preview.registration_mode, Some(RegistrationMode::Allowlist));
    // Nothing is pinned before the user confirms.
    assert!(h.servers.list().await.unwrap().is_empty());

    let profile = h.servers.confirm(preview.preview_id).await.unwrap();
    assert!(profile.active);
    assert_eq!(profile.fingerprint, preview.fingerprint);
    assert!(profile.account.is_none());
    let trust = h.servers.trust_state(server_id()).await.unwrap().unwrap();
    assert_eq!(trust.version(), 3);
    assert!(h.events().iter().any(|e| matches!(
        e,
        AppEvent::ServerSwitched(ServerSwitched { server_id: Some(id) }) if *id == server_id()
    )));

    // A preview is single use, and the same server cannot be added twice.
    assert_eq!(
        h.servers.confirm(preview.preview_id).await.unwrap_err(),
        ServerError::PreviewExpired
    );
    assert_eq!(
        h.servers.preview(&h.mock.uri(), None).await.unwrap_err(),
        ServerError::AlreadyAdded {
            server_id: server_id()
        }
    );
}

#[tokio::test]
async fn a_link_fingerprint_must_match_the_server() {
    let h = Harness::new().await;
    let root = key(1);
    h.serve_identity(&root).await;
    let other = key(2).public_key().fingerprint().to_string();
    assert_eq!(
        h.servers
            .preview(&h.mock.uri(), Some(&other))
            .await
            .unwrap_err(),
        ServerError::FingerprintMismatch {
            expected: other,
            actual: root.public_key().fingerprint().to_string()
        }
    );
    let right = root.public_key().fingerprint().to_string();
    let preview = h
        .servers
        .preview(&h.mock.uri(), Some(&right.to_ascii_lowercase()))
        .await
        .unwrap();
    assert_eq!(
        preview.expected_fingerprint.as_deref(),
        Some(right.as_str())
    );
}

#[tokio::test]
async fn a_changed_root_key_blocks_the_server_without_bypass() {
    let mut h = Harness::new().await;
    let root = key(1);
    let impostor = key(9);
    h.add(&root).await;
    h.events();

    h.reset().await;
    h.serve_identity(&impostor).await;
    h.serve_bundle(bundle(&impostor, 5, None)).await;
    let error = h.servers.connect(server_id()).await.unwrap_err();
    assert!(matches!(error, ConnectError::Blocked { .. }), "{error}");
    let presented = impostor.public_key().fingerprint().to_string();
    assert!(h.events().iter().any(|e| matches!(
        e,
        AppEvent::TrustProblem(p) if p.presented_fingerprint == presented
            && p.pinned_fingerprint == root.public_key().fingerprint().to_string()
            && p.kind == TrustProblemKind::FingerprintMismatch
    )));

    // Every request is refused, including sign-in and trust refresh.
    let api = h.servers.api(server_id()).await.unwrap();
    let result: Result<Value, _> = api.public(Method::GET, "v1/packages", None::<&()>).await;
    assert_eq!(result.unwrap_err(), ApiError::TrustBlocked);
    assert!(matches!(
        h.servers.refresh_trust(server_id()).await,
        Err(TrustRefreshError::Api(ApiError::TrustBlocked))
    ));
    assert!(matches!(
        h.servers.auth_start(server_id()).await,
        Err(AuthError::Server { code, .. }) if code == "trust_blocked"
    ));
    assert!(h.servers.trust_state(server_id()).await.unwrap().is_none());
    // Re-adding the server cannot replace the pin.
    assert!(matches!(
        h.servers.preview(&h.mock.uri(), None).await,
        Err(ServerError::FingerprintMismatch { .. })
    ));
    let listed = h.servers.list().await.unwrap();
    assert_eq!(
        listed[0].blocked_fingerprint.as_deref(),
        Some(presented.as_str())
    );
    assert_eq!(
        listed[0].fingerprint,
        root.public_key().fingerprint().to_string()
    );

    // The block survives a restart.
    let h = h.restart();
    let api = h.servers.api(server_id()).await.unwrap();
    let result: Result<Value, _> = api.public(Method::GET, "v1/packages", None::<&()>).await;
    assert_eq!(result.unwrap_err(), ApiError::TrustBlocked);

    // It lifts only when the server presents the pinned key again.
    h.reset().await;
    h.serve_identity(&root).await;
    h.serve_bundle(bundle(&root, 1, None)).await;
    h.servers.connect(server_id()).await.unwrap();
    assert!(
        h.servers.list().await.unwrap()[0]
            .blocked_fingerprint
            .is_none()
    );
    assert!(h.servers.trust_state(server_id()).await.unwrap().is_some());
}

#[tokio::test]
async fn trust_bundle_rollback_and_forgeries_are_refused() {
    let mut h = Harness::new().await;
    let root = key(1);
    h.add(&root).await;

    h.reset().await;
    h.serve_bundle(bundle(&root, 4, None)).await;
    let state = h.servers.refresh_trust(server_id()).await.unwrap().unwrap();
    assert_eq!(state.version(), 4);

    h.reset().await;
    h.serve_bundle(bundle(&root, 2, None)).await;
    assert!(matches!(
        h.servers.refresh_trust(server_id()).await,
        Err(TrustRefreshError::Refused(TrustError::Rollback {
            last_seen: 4,
            found: 2
        }))
    ));

    h.reset().await;
    h.serve_bundle(bundle(&key(7), 9, None)).await;
    assert!(matches!(
        h.servers.refresh_trust(server_id()).await,
        Err(TrustRefreshError::Refused(TrustError::Signature))
    ));

    // The stored bundle is still the highest verified one, also after a restart.
    let h = h.restart();
    let state = h.servers.trust_state(server_id()).await.unwrap().unwrap();
    assert_eq!(state.version(), 4);
}

#[tokio::test]
async fn an_announced_next_root_completes_a_rotation() {
    let mut h = Harness::new().await;
    let old = key(1);
    let new = key(2);
    h.add(&old).await;

    h.reset().await;
    h.serve_bundle(bundle(&old, 2, Some(&new))).await;
    h.servers.refresh_trust(server_id()).await.unwrap();

    // The server switches to the new root: its identity is accepted (it was
    // announced by a bundle the old root signed) and the pin moves.
    h.reset().await;
    h.serve_identity(&new).await;
    h.serve_bundle(bundle(&new, 3, None)).await;
    h.servers.connect(server_id()).await.unwrap();
    let profile = h.servers.profile(server_id()).await.unwrap().unwrap();
    assert_eq!(
        profile.fingerprint,
        new.public_key().fingerprint().to_string()
    );
    assert!(profile.blocked_fingerprint.is_none());
    assert_eq!(
        h.servers
            .trust_state(server_id())
            .await
            .unwrap()
            .unwrap()
            .version(),
        3
    );

    // The old root is no longer trusted.
    h.reset().await;
    h.serve_identity(&old).await;
    assert!(matches!(
        h.servers.connect(server_id()).await,
        Err(ConnectError::Blocked { .. })
    ));
}

/// Signs in through the deep-link path with tokens `vga_1` / `vgr_1`.
async fn sign_in(h: &mut Harness) -> Account {
    Mock::given(method("POST"))
        .and(path("/v1/auth/discord/start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorize_url": "https://discord.example/oauth2/authorize?state=s",
            "expires_at": "2099-01-01T00:00:00Z"
        })))
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/token"))
        .and(body_partial_json(
            json!({"grant_type": "authorization_code", "code": "login-code"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_response("vga_1", "vgr_1")))
        .mount(&h.mock)
        .await;

    let flow = h.servers.auth_start(server_id()).await.unwrap();
    assert!(flow.browser_opened);
    assert_eq!(
        h.browser.opened.lock().unwrap()[0].as_str(),
        "https://discord.example/oauth2/authorize?state=s"
    );
    let start = body_json(&requests_to(&h.mock, "/v1/auth/discord/start").await[0]);
    assert_eq!(start["client"], "desktop");
    assert_eq!(start["device_name"], "test-pc");
    let client_state = start["client_state"].as_str().unwrap().to_owned();

    // A callback for an unknown flow does nothing.
    h.servers.auth_callback("login-code", "someone-else").await;
    assert!(requests_to(&h.mock, "/v1/auth/token").await.is_empty());

    h.servers.auth_callback("login-code", &client_state).await;
    let exchange = body_json(&requests_to(&h.mock, "/v1/auth/token").await[0]);
    let verifier = exchange["code_verifier"].as_str().unwrap();
    assert_eq!(pkce_challenge(verifier), start["code_challenge"]);
    let account = h
        .events()
        .into_iter()
        .find_map(|e| match e {
            AppEvent::AuthFinished(AuthFinished {
                flow_id,
                outcome: AuthOutcome::SignedIn { account },
            }) if flow_id == flow.flow_id => Some(account),
            _ => None,
        })
        .expect("signed in");
    h.mock.reset().await;
    account
}

#[tokio::test]
async fn sign_in_stores_tokens_in_the_vault_and_the_account_locally() {
    let mut h = Harness::new().await;
    h.add(&key(1)).await;
    let account = sign_in(&mut h).await;
    assert_eq!(account.username, "alice");
    assert_eq!(account.display_name.as_deref(), Some("Alice"));
    assert_eq!(account.role, Role::Admin);
    assert_eq!(h.vault.len(), 1);
    let stored = h
        .vault
        .get(&format!("tokens:{SERVER_ID}"))
        .unwrap()
        .unwrap();
    assert!(stored.contains("vgr_1"));
    let profile = h.servers.profile(server_id()).await.unwrap().unwrap();
    assert_eq!(profile.account, Some(account));
    // The same callback cannot be replayed.
    h.servers.auth_callback("login-code", "anything").await;
    assert!(requests_to(&h.mock, "/v1/auth/token").await.is_empty());
}

#[tokio::test]
async fn refresh_rotates_the_token_once_for_concurrent_callers() {
    let mut h = Harness::new().await;
    h.add(&key(1)).await;
    sign_in(&mut h).await;
    // After a restart the access token's expiry is unknown; the server says 401.
    let h = h.restart();

    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .and(header("authorization", "Bearer vga_1"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"code": "session_expired"})))
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/token"))
        .and(body_partial_json(
            json!({"grant_type": "refresh_token", "refresh_token": "vgr_1"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(token_response("vga_2", "vgr_2"))
                .set_delay(Duration::from_millis(100)),
        )
        .expect(1)
        .mount(&h.mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .and(header("authorization", "Bearer vga_2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&h.mock)
        .await;

    let api = h.servers.api(server_id()).await.unwrap();
    let calls = (0..5).map(|_| {
        let api = api.clone();
        async move { api.authed::<(), Value>(Method::GET, "v1/me", None).await }
    });
    for result in futures_util::future::join_all(calls).await {
        assert_eq!(result.unwrap(), json!({"ok": true}));
    }
    let stored = h
        .vault
        .get(&format!("tokens:{SERVER_ID}"))
        .unwrap()
        .unwrap();
    assert!(stored.contains("vgr_2") && !stored.contains("vgr_1"));
    h.mock.verify().await;
}

#[tokio::test]
async fn a_401_refreshes_once_and_retries_once() {
    let mut h = Harness::new().await;
    h.add(&key(1)).await;
    sign_in(&mut h).await;

    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"code": "unauthenticated"})))
        .expect(2)
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_response("vga_2", "vgr_2")))
        .expect(1)
        .mount(&h.mock)
        .await;

    let api = h.servers.api(server_id()).await.unwrap();
    let result = api.authed::<(), Value>(Method::GET, "v1/me", None).await;
    assert_eq!(result.unwrap_err(), ApiError::Unauthenticated);
    h.mock.verify().await;
}

#[tokio::test]
async fn a_refused_refresh_signs_out_locally() {
    let mut h = Harness::new().await;
    h.add(&key(1)).await;
    sign_in(&mut h).await;
    let mut events = h.bus.subscribe();

    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/token"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"code": "refresh_token_reused"})),
        )
        .expect(1)
        .mount(&h.mock)
        .await;

    let api = h.servers.api(server_id()).await.unwrap();
    let result = api.authed::<(), Value>(Method::GET, "v1/me", None).await;
    assert_eq!(result.unwrap_err(), ApiError::Unauthenticated);
    assert!(h.vault.is_empty());
    // The account row goes too (asynchronously, then `ServersChanged`).
    loop {
        if let AppEvent::ServersChanged(_) = events.recv().await.unwrap() {
            break;
        }
    }
    let profile = h.servers.profile(server_id()).await.unwrap().unwrap();
    assert!(profile.account.is_none());
    // Later calls do not reach the server.
    let result = api.authed::<(), Value>(Method::GET, "v1/me", None).await;
    assert_eq!(result.unwrap_err(), ApiError::Unauthenticated);
    h.mock.verify().await;
}

#[tokio::test]
async fn the_paste_code_fallback_checks_code_and_state() {
    let mut h = Harness::new().await;
    h.add(&key(1)).await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/discord/start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorize_url": "javascript:alert(1)",
            "expires_at": "2099-01-01T00:00:00Z"
        })))
        .up_to_n_times(1)
        .mount(&h.mock)
        .await;
    // A non-web authorize URL is never opened.
    assert!(matches!(
        h.servers.auth_start(server_id()).await,
        Err(AuthError::Server { .. })
    ));
    assert!(h.browser.opened.lock().unwrap().is_empty());

    Mock::given(method("POST"))
        .and(path("/v1/auth/discord/start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorize_url": "https://discord.example/oauth2/authorize",
            "expires_at": "2099-01-01T00:00:00Z"
        })))
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"code": "invalid_grant"})))
        .mount(&h.mock)
        .await;

    let flow = h.servers.auth_start(server_id()).await.unwrap();
    assert_eq!(
        h.servers
            .auth_submit_code(flow.flow_id, "not a code!")
            .await
            .unwrap_err(),
        AuthError::InvalidCode
    );
    // A pasted link with another flow's state is refused without a request.
    let link = "vgames://auth/callback?code=abc&client_state=wrongstatewrongstate00";
    assert_eq!(
        h.servers
            .auth_submit_code(flow.flow_id, link)
            .await
            .unwrap_err(),
        AuthError::InvalidCode
    );
    assert!(requests_to(&h.mock, "/v1/auth/token").await.is_empty());
    // The flow was consumed by that attempt.
    assert_eq!(
        h.servers
            .auth_submit_code(flow.flow_id, "abc")
            .await
            .unwrap_err(),
        AuthError::Expired
    );

    let flow = h.servers.auth_start(server_id()).await.unwrap();
    assert_eq!(
        h.servers
            .auth_submit_code(flow.flow_id, " abc ")
            .await
            .unwrap_err(),
        AuthError::InvalidCode
    );
    assert_eq!(requests_to(&h.mock, "/v1/auth/token").await.len(), 1);

    let flow = h.servers.auth_start(server_id()).await.unwrap();
    h.events();
    h.servers.auth_cancel(flow.flow_id);
    assert!(h.events().iter().any(|e| matches!(
        e,
        AppEvent::AuthFinished(AuthFinished {
            outcome: AuthOutcome::Failed {
                error: AuthError::Cancelled
            },
            ..
        })
    )));
    assert_eq!(
        h.servers.auth_open_browser(flow.flow_id).unwrap_err(),
        AuthError::Expired
    );
}

#[tokio::test]
async fn sign_out_revokes_and_forgets_the_session() {
    let mut h = Harness::new().await;
    h.add(&key(1)).await;
    sign_in(&mut h).await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/logout"))
        .and(header("authorization", "Bearer vga_1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&h.mock)
        .await;
    h.servers.sign_out(server_id()).await.unwrap();
    assert!(h.vault.is_empty());
    assert!(
        h.servers
            .profile(server_id())
            .await
            .unwrap()
            .unwrap()
            .account
            .is_none()
    );
    h.mock.verify().await;
}

#[tokio::test]
async fn servers_switch_and_remove() {
    let mut h = Harness::new().await;
    h.add(&key(1)).await;
    assert_eq!(
        h.servers.switch(Uuid::now_v7()).await.unwrap_err(),
        AppError::NotFound
    );
    h.events();
    h.servers.remove(server_id()).await.unwrap();
    assert!(h.servers.list().await.unwrap().is_empty());
    assert!(h.events().iter().any(|e| matches!(
        e,
        AppEvent::ServerSwitched(ServerSwitched { server_id: None })
    )));
    assert_eq!(
        h.servers.remove(server_id()).await.unwrap_err(),
        AppError::NotFound
    );
}

#[tokio::test]
async fn discovery_failures_are_typed() {
    let h = Harness::new().await;
    Mock::given(method("GET"))
        .and(path("/.well-known/vgames.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(70 * 1024)))
        .mount(&h.mock)
        .await;
    assert_eq!(
        h.servers.preview(&h.mock.uri(), None).await.unwrap_err(),
        ServerError::NotVgames
    );
    h.reset().await;
    Mock::given(method("GET"))
        .and(path("/.well-known/vgames.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.mock)
        .await;
    assert_eq!(
        h.servers.preview(&h.mock.uri(), None).await.unwrap_err(),
        ServerError::NotVgames
    );
    assert_eq!(
        h.servers
            .preview("http://games.example.com", None)
            .await
            .unwrap_err(),
        ServerError::InsecureScheme
    );
    // Nothing listens on port 9 of the loopback address.
    assert!(matches!(
        h.servers.preview("http://127.0.0.1:9", None).await,
        Err(ServerError::Unreachable { .. })
    ));
}
