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
        serve_identity_at(&self.mock, root, SERVER_ID).await;
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

/// Serves `/.well-known/vgames.json` for the server `id` with `root` on `mock`.
async fn serve_identity_at(mock: &MockServer, root: &SecretKey, id: &str) {
    let key = root.public_key();
    Mock::given(method("GET"))
        .and(path("/.well-known/vgames.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "format": "vgames.server/1",
            "server_id": id,
            "name": "Friends",
            "motd": "Welcome",
            "api_versions": ["v1"],
            "root_public_key": key.to_base64(),
            "root_key_fingerprint": key.fingerprint().to_string(),
            "registration_mode": "allowlist",
            "features": ["social"],
            "min_launcher_version": "0.1.0"
        })))
        .mount(mock)
        .await;
}

/// A signed `vgames.trust/1` bundle as served by `GET /v1/trust/bundle`.
fn bundle(signer: &SecretKey, version: u64, next_root: Option<&SecretKey>) -> Value {
    bundle_for(signer, version, next_root, SERVER_ID)
}

fn bundle_for(signer: &SecretKey, version: u64, next_root: Option<&SecretKey>, id: &str) -> Value {
    let next_root = next_root.map(|k| {
        let key = k.public_key();
        json!({ "public_key": key.to_base64(), "key_id": key.key_id().to_hex() })
    });
    let bytes = serde_json::to_vec(&json!({
        "format": "vgames.trust/1",
        "server_id": id,
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
    sign_in_at(
        &h.servers,
        &mut h.events,
        &h.browser,
        &h.mock,
        server_id(),
        ("vga_1", "vgr_1"),
    )
    .await
}

/// Signs in to the server `id` served by `mock`, which answers with `tokens` (access, refresh).
async fn sign_in_at(
    servers: &Servers,
    events: &mut broadcast::Receiver<AppEvent>,
    browser: &RecordingBrowser,
    mock: &MockServer,
    id: Uuid,
    (access, refresh): (&str, &str),
) -> Account {
    Mock::given(method("POST"))
        .and(path("/v1/auth/discord/start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorize_url": "https://discord.example/oauth2/authorize?state=s",
            "expires_at": "2099-01-01T00:00:00Z"
        })))
        .mount(mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/token"))
        .and(body_partial_json(
            json!({"grant_type": "authorization_code", "code": "login-code"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_response(access, refresh)))
        .mount(mock)
        .await;

    let flow = servers.auth_start(id).await.unwrap();
    assert!(flow.browser_opened);
    assert_eq!(
        browser.opened.lock().unwrap().last().unwrap().as_str(),
        "https://discord.example/oauth2/authorize?state=s"
    );
    let start = body_json(&requests_to(mock, "/v1/auth/discord/start").await[0]);
    assert_eq!(start["client"], "desktop");
    assert_eq!(start["device_name"], "test-pc");
    let client_state = start["client_state"].as_str().unwrap().to_owned();

    // A callback for an unknown flow does nothing.
    servers.auth_callback("login-code", "someone-else").await;
    assert!(requests_to(mock, "/v1/auth/token").await.is_empty());

    servers.auth_callback("login-code", &client_state).await;
    let exchange = body_json(&requests_to(mock, "/v1/auth/token").await[0]);
    let verifier = exchange["code_verifier"].as_str().unwrap();
    assert_eq!(pkce_challenge(verifier), start["code_challenge"]);
    let mut account = None;
    while let Ok(event) = events.try_recv() {
        if let AppEvent::AuthFinished(AuthFinished {
            flow_id,
            outcome: AuthOutcome::SignedIn { account: signed_in },
        }) = event
            && flow_id == flow.flow_id
        {
            account = Some(signed_in);
        }
    }
    mock.reset().await;
    account.expect("signed in")
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
async fn a_refused_sign_in_link_ends_only_its_own_flow() {
    let mut h = Harness::new().await;
    h.add(&key(1)).await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/discord/start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorize_url": "https://discord.example/oauth2/authorize",
            "expires_at": "2099-01-01T00:00:00Z"
        })))
        .mount(&h.mock)
        .await;
    let flow = h.servers.auth_start(server_id()).await.unwrap();
    let start = body_json(&requests_to(&h.mock, "/v1/auth/discord/start").await[0]);
    let client_state = start["client_state"].as_str().unwrap().to_owned();
    h.events();

    // Another flow's state is ignored, and this flow stays usable.
    h.servers
        .auth_callback_error("not_allowlisted", "someone-else");
    assert!(h.events().is_empty());
    assert!(h.servers.auth_open_browser(flow.flow_id).is_ok());

    h.servers
        .auth_callback_error("not_allowlisted", &client_state);
    assert!(h.events().iter().any(|e| matches!(
        e,
        AppEvent::AuthFinished(AuthFinished {
            flow_id,
            outcome: AuthOutcome::Failed {
                error: AuthError::NotAllowlisted
            },
        }) if *flow_id == flow.flow_id
    )));
    // The flow is gone: no code exchange can follow.
    assert_eq!(
        h.servers.auth_open_browser(flow.flow_id).unwrap_err(),
        AuthError::Expired
    );
    assert!(requests_to(&h.mock, "/v1/auth/token").await.is_empty());
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

// ------------------------------------------------------------------------------------------------
// INS-05: the reusable auth hook and the account commands.

#[tokio::test]
async fn the_auth_hook_retries_a_custom_request_once_with_the_refreshed_token() {
    let mut h = Harness::new().await;
    h.add(&key(1)).await;
    sign_in(&mut h).await;
    Mock::given(method("GET"))
        .and(path("/v1/packages"))
        .and(header("authorization", "Bearer vga_1"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_response("vga_2", "vgr_2")))
        .expect(1)
        .mount(&h.mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/packages"))
        .and(header("authorization", "Bearer vga_2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [] })))
        .expect(1)
        .mount(&h.mock)
        .await;

    let api = h.servers.api(server_id()).await.unwrap();
    let url = api.url("v1/packages").unwrap();
    let response = api
        .with_auth(&Method::GET, |http, token| {
            http.get(url.clone())
                .query(&[("q", "orbit")])
                .bearer_auth(token)
        })
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let requests = requests_to(&h.mock, "/v1/packages").await;
    assert!(requests.iter().all(|r| r.url.query() == Some("q=orbit")));
    h.mock.verify().await;
}

fn session_json(id: &str, kind: &str, current: bool) -> Value {
    json!({
        "id": id,
        "kind": kind,
        "device_name": if kind == "desktop" { json!("test-pc") } else { Value::Null },
        "user_agent": "Firefox on Windows",
        "created_at": "2026-09-24T10:00:00Z",
        "last_used_at": "2026-09-26T10:00:00Z",
        "current": current
    })
}

const OUR_SESSION: &str = "01920000-0000-7000-8000-0000000005e1";
const WEB_SESSION: &str = "01920000-0000-7000-8000-0000000005e2";

async fn serve_sessions(h: &Harness) {
    Mock::given(method("GET"))
        .and(path("/v1/me/sessions"))
        .and(header("authorization", "Bearer vga_1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [session_json(OUR_SESSION, "desktop", true), session_json(WEB_SESSION, "web", false)]
        })))
        .mount(&h.mock)
        .await;
}

#[tokio::test]
async fn account_sessions_lists_where_the_account_is_signed_in() {
    let mut h = Harness::new().await;
    h.add(&key(1)).await;
    sign_in(&mut h).await;
    serve_sessions(&h).await;
    let sessions = crate::commands::account::sessions(&h.servers, server_id())
        .await
        .unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].id, OUR_SESSION);
    assert!(sessions[0].current);
    assert_eq!(sessions[0].device_name.as_deref(), Some("test-pc"));
    assert_eq!(
        sessions[1].client,
        crate::commands::account::SessionClient::Web
    );
    assert_eq!(sessions[1].last_used_at, "2026-09-26T10:00:00Z");
    assert!(!sessions[1].current);
}

#[tokio::test]
async fn revoking_another_session_keeps_this_launcher_signed_in() {
    let mut h = Harness::new().await;
    h.add(&key(1)).await;
    sign_in(&mut h).await;
    serve_sessions(&h).await;
    Mock::given(method("DELETE"))
        .and(path(format!("/v1/me/sessions/{WEB_SESSION}")))
        .and(header("authorization", "Bearer vga_1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&h.mock)
        .await;
    let web = Uuid::parse_str(WEB_SESSION).unwrap();
    crate::commands::account::revoke(&h.servers, server_id(), web)
        .await
        .unwrap();
    assert_eq!(h.vault.len(), 1);
    let profile = h.servers.profile(server_id()).await.unwrap().unwrap();
    assert!(profile.account.is_some());
    h.mock.verify().await;
}

#[tokio::test]
async fn revoking_this_launchers_session_signs_out_locally_without_logout() {
    let mut h = Harness::new().await;
    h.add(&key(1)).await;
    sign_in(&mut h).await;
    serve_sessions(&h).await;
    Mock::given(method("DELETE"))
        .and(path(format!("/v1/me/sessions/{OUR_SESSION}")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/logout"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&h.mock)
        .await;
    let ours = Uuid::parse_str(OUR_SESSION).unwrap();
    crate::commands::account::revoke(&h.servers, server_id(), ours)
        .await
        .unwrap();
    assert!(h.vault.is_empty());
    let profile = h.servers.profile(server_id()).await.unwrap().unwrap();
    assert!(profile.account.is_none());
    h.mock.verify().await;
}

#[tokio::test]
async fn revoking_an_unknown_session_is_not_found_and_keeps_the_account() {
    let mut h = Harness::new().await;
    h.add(&key(1)).await;
    sign_in(&mut h).await;
    serve_sessions(&h).await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"code": "not_found"})))
        .mount(&h.mock)
        .await;
    let unknown = Uuid::parse_str("01920000-0000-7000-8000-0000000005ff").unwrap();
    assert_eq!(
        crate::commands::account::revoke(&h.servers, server_id(), unknown)
            .await
            .unwrap_err(),
        AppError::NotFound
    );
    assert_eq!(h.vault.len(), 1);
}

// ------------------------------------------------------------------------------------------------
// INS-05: tokens never cross servers (docs/security/test-matrix.md, "Compromised server → read
// tokens for other servers").

const OTHER_ID: &str = "01920000-0000-7000-8000-000000000002";

/// Answers `GET /v1/me` with 401 once for `stale`, then 200 for `fresh`, and rotates the
/// refresh token `refresh` to (`fresh`, `next_refresh`).
async fn serve_me_with_one_refresh(
    mock: &MockServer,
    (stale, fresh): (&str, &str),
    (refresh, next_refresh): (&str, &str),
) {
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .and(header("authorization", format!("Bearer {stale}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": stale})))
        .mount(mock)
        .await;
    // Higher priority than the 200 above: the stale token is refused once.
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .and(header("authorization", format!("Bearer {stale}").as_str()))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"code": "session_expired"})))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/token"))
        .and(body_partial_json(
            json!({"grant_type": "refresh_token", "refresh_token": refresh}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_response(fresh, next_refresh)))
        .expect(1)
        .mount(mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .and(header("authorization", format!("Bearer {fresh}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": fresh})))
        .mount(mock)
        .await;
}

/// Every token-looking string in what `mock` received: bearer headers and token fields of
/// JSON bodies.
async fn tokens_received(mock: &MockServer) -> Vec<String> {
    let mut tokens = Vec::new();
    for request in mock.received_requests().await.unwrap() {
        for (name, value) in &request.headers {
            if name.as_str().eq_ignore_ascii_case("authorization") {
                tokens.push(value.to_str().unwrap().to_owned());
            }
        }
        if let Ok(body) = serde_json::from_slice::<Value>(&request.body) {
            for field in ["refresh_token", "access_token", "token"] {
                if let Some(token) = body[field].as_str() {
                    tokens.push(token.to_owned());
                }
            }
        }
    }
    tokens
}

#[tokio::test]
async fn a_request_to_one_server_never_carries_another_servers_token() {
    // Two servers, both signed in, with their own tokens.
    let mut h = Harness::new().await;
    h.add(&key(1)).await;
    sign_in(&mut h).await;
    let other = MockServer::start().await;
    let other_id = Uuid::parse_str(OTHER_ID).unwrap();
    serve_identity_at(&other, &key(2), OTHER_ID).await;
    Mock::given(method("GET"))
        .and(path("/v1/trust/bundle"))
        .respond_with(ResponseTemplate::new(200).set_body_json(bundle_for(
            &key(2),
            1,
            None,
            OTHER_ID,
        )))
        .mount(&other)
        .await;
    let preview = h.servers.preview(&other.uri(), None).await.unwrap();
    h.servers.confirm(preview.preview_id).await.unwrap();
    sign_in_at(
        &h.servers,
        &mut h.events,
        &h.browser,
        &other,
        other_id,
        ("vgb_1", "vgrb_1"),
    )
    .await;
    assert_eq!(h.vault.len(), 2);

    // Each server refuses its first access token once, so each refreshes; the switches
    // (and the reconnect a switch triggers) happen in between.
    h.serve_identity(&key(1)).await;
    h.serve_bundle(bundle(&key(1), 1, None)).await;
    serve_me_with_one_refresh(&h.mock, ("vga_1", "vga_2"), ("vgr_1", "vgr_2")).await;
    serve_identity_at(&other, &key(2), OTHER_ID).await;
    Mock::given(method("GET"))
        .and(path("/v1/trust/bundle"))
        .respond_with(ResponseTemplate::new(200).set_body_json(bundle_for(
            &key(2),
            1,
            None,
            OTHER_ID,
        )))
        .mount(&other)
        .await;
    serve_me_with_one_refresh(&other, ("vgb_1", "vgb_2"), ("vgrb_1", "vgrb_2")).await;

    let me = |id| {
        let servers = &h.servers;
        async move {
            let api = servers.api(id).await.unwrap();
            api.authed::<(), Value>(Method::GET, "v1/me", None)
                .await
                .unwrap()
        }
    };
    assert_eq!(me(server_id()).await, json!({"ok": "vga_2"}));
    h.servers.switch(other_id).await.unwrap();
    h.servers.connect(other_id).await.unwrap();
    assert_eq!(me(other_id).await, json!({"ok": "vgb_2"}));
    assert_eq!(me(server_id()).await, json!({"ok": "vga_2"}));
    h.servers.switch(server_id()).await.unwrap();
    h.servers.connect(server_id()).await.unwrap();
    assert_eq!(me(server_id()).await, json!({"ok": "vga_2"}));
    assert_eq!(me(other_id).await, json!({"ok": "vgb_2"}));

    // After a restart the sessions come back from the vault, each under its own server.
    let mut h = h.restart();
    h.events();
    let me = |id| {
        let servers = &h.servers;
        async move {
            let api = servers.api(id).await.unwrap();
            api.authed::<(), Value>(Method::GET, "v1/me", None)
                .await
                .unwrap()
        }
    };
    assert_eq!(me(other_id).await, json!({"ok": "vgb_2"}));
    assert_eq!(me(server_id()).await, json!({"ok": "vga_2"}));

    let ours = tokens_received(&h.mock).await;
    let theirs = tokens_received(&other).await;
    assert!(
        ours.iter().any(|t| t.contains("vgr_1")) && theirs.iter().any(|t| t.contains("vgrb_1"))
    );
    for token in &ours {
        assert!(
            token.contains("vga_") || token.contains("vgr_"),
            "{token} reached the first server"
        );
    }
    for token in &theirs {
        assert!(
            token.contains("vgb_") || token.contains("vgrb_"),
            "{token} reached the second server"
        );
    }
    h.mock.verify().await;
    other.verify().await;
}
