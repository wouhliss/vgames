//! Server commands (A5-T06) against a local mock of the vgames API: `login`
//! (PKCE + pasted code), token refresh, `logout`, `trust publish` and
//! `trust re-sign`, ending with a launcher-side `verify_manifest` of the
//! re-signed release under the new bundle.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;
use vgames_core::Digest;
use vgames_core::keyfile::{KeyFile, KeyKind};
use vgames_core::sign::{Context, Envelope, SecretKey};
use vgames_core::trust::{RootPin, SignedBundle, sign_bundle, verify_bundle};
use vgames_core::verify::{ExpectedRelease, VerifyError, VerifyMode, verify_manifest};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const SERVER_ID: &str = "01920000-0000-7000-8000-000000000000";
const HOLDER: &str = "0192aaaa-0000-7000-8000-000000000001";
const PACKAGE: &str = "0192a6f0-1c2d-7e3f-8a9b-0c1d2e3f4a5b";
/// The manifest vector's version (current Windows release, signed by the old key).
const CURRENT: &str = "0192a6f1-aaaa-7bbb-8ccc-dddddddddddd";
const OLDER: &str = "0192a6f1-aaaa-7bbb-8ccc-000000000002";
const FORGED: &str = "0192a6f1-aaaa-7bbb-8ccc-000000000003";
const OTHER_KEY: &str = "0192a6f1-aaaa-7bbb-8ccc-000000000004";
const PASSPHRASE: &str = "correct horse battery staple 42";
const MANIFEST: &[u8] = include_bytes!("../../vgames-core/tests/vectors/pack_manifest.json");

fn root() -> SecretKey {
    SecretKey::from_seed(&[1; 32])
}
fn old_key() -> SecretKey {
    SecretKey::from_seed(&[2; 32])
}
fn new_key() -> SecretKey {
    SecretKey::from_seed(&[3; 32])
}

fn token(prefix: &str, fill: char) -> String {
    format!("{prefix}{}", fill.to_string().repeat(43))
}

fn publisher(key: &SecretKey) -> Value {
    let pk = key.public_key();
    json!({ "key_id": pk.key_id(), "public_key": pk, "holder_user_id": HOLDER, "label": "alice",
            "not_before": "2026-09-24T00:00:00Z", "not_after": "2028-09-24T00:00:00Z" })
}

fn bundle(
    version: u64,
    publishers: &[SecretKey],
    revoked: &[SecretKey],
    signer: &SecretKey,
    server_id: &str,
) -> SignedBundle {
    let doc = json!({
        "format": "vgames.trust/1", "server_id": server_id, "version": version,
        "issued_at": "2026-09-24T10:00:00Z", "expires_at": null,
        "root_key_id": root().public_key().key_id(),
        "publishers": publishers.iter().map(publisher).collect::<Vec<_>>(),
        "revoked": revoked.iter().map(|k| json!({ "key_id": k.public_key().key_id(),
            "revoked_at": "2026-09-25T08:00:00Z", "reason": "laptop stolen" })).collect::<Vec<_>>(),
        "next_root": null
    });
    let bytes = serde_json::to_vec(&doc).unwrap();
    SignedBundle::new(&bytes, sign_bundle(signer, &bytes))
}

fn server_info(root: &SecretKey) -> Value {
    json!({ "format": "vgames.server/1", "server_id": SERVER_ID, "name": "Test server",
            "api_versions": ["v1"], "root_public_key": root.public_key().to_base64(),
            "root_key_fingerprint": root.public_key().fingerprint().to_string(), "features": [] })
}

fn tokens(access: &str, refresh: &str, expires_in: i64) -> Value {
    json!({ "access_token": access, "refresh_token": refresh, "token_type": "Bearer",
            "expires_in": expires_in,
            "user": { "id": HOLDER, "username": "alice", "role": "owner",
                      "created_at": "2026-09-24T10:00:00Z" } })
}

struct Env {
    server: MockServer,
    dir: TempDir,
}

impl Env {
    async fn start() -> Self {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/vgames.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(server_info(&root())))
            .mount(&server)
            .await;
        Self {
            server,
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn uri(&self) -> String {
        self.server.uri()
    }

    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let p = self.dir.path().join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn config(&self) -> PathBuf {
        self.dir.path().join("config")
    }

    /// Runs `vgames args…` with the file credential store in the temp dir.
    async fn run(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_vgames"));
        cmd.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("VGAMES_CREDENTIAL_STORE", "file")
            .env("VGAMES_CONFIG_DIR", self.config())
            .env("VGAMES_TEST_PASS", PASSPHRASE)
            .args(args);
        for (k, v) in envs {
            cmd.env(k, v);
        }
        tokio::task::spawn_blocking(move || cmd.output().unwrap())
            .await
            .unwrap()
    }
}

fn text(o: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        o.status,
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

// ---------------------------------------------------------------------------
// login

/// `POST /v1/auth/discord/start`: remembers the PKCE challenge and client_state.
#[derive(Clone, Default)]
struct Start(Arc<Mutex<Option<(String, String)>>>);

impl Respond for Start {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = request.body_json().unwrap();
        assert_eq!(body["client"], "desktop");
        let challenge = body["code_challenge"].as_str().unwrap().to_owned();
        let state = body["client_state"].as_str().unwrap().to_owned();
        let url = format!("https://discord.example/oauth2/authorize?cs={state}");
        *self.0.lock().unwrap() = Some((challenge, state));
        ResponseTemplate::new(200)
            .set_body_json(json!({ "authorize_url": url, "expires_at": "2030-01-01T00:00:00Z" }))
    }
}

const LOGIN_CODE: &str = "LoginCodeLoginCodeLoginCodeLoginCodeLoginCo";

/// `POST /v1/auth/token` with a login code: checks S256(verifier) == challenge.
#[derive(Clone)]
struct Exchange {
    start: Start,
    expires_in: i64,
}

impl Respond for Exchange {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = request.body_json().unwrap();
        let Some((challenge, _)) = self.start.0.lock().unwrap().clone() else {
            return ResponseTemplate::new(400);
        };
        let verifier = body["code_verifier"].as_str().unwrap_or_default();
        let s256 = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        if body["grant_type"] != "authorization_code"
            || body["code"] != LOGIN_CODE
            || s256 != challenge
        {
            return ResponseTemplate::new(401).set_body_json(json!({
                "type": "urn:vgames:problem:invalid_grant", "title": "Invalid grant",
                "status": 401, "code": "invalid_grant" }));
        }
        ResponseTemplate::new(200).set_body_json(tokens(
            &token("vga_", 'a'),
            &token("vgr_", 'r'),
            self.expires_in,
        ))
    }
}

/// A stand-in for the browser: prints the callback link for the sign-in URL.
fn helper(env: &Env, client_state: Option<&str>) -> PathBuf {
    let state = client_state.map_or_else(
        || r#"$(printf '%s\n' "$1" | sed -n 's/.*[?&]cs=\([A-Za-z0-9_-]*\).*/\1/p')"#.to_owned(),
        str::to_owned,
    );
    let script = format!(
        "#!/bin/sh\nprintf 'vgames://auth/callback?code={LOGIN_CODE}&client_state=%s\\n' \"{state}\"\n"
    );
    let p = env.file("browser.sh", script.as_bytes());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

async fn mount_login(env: &Env, expires_in: i64) -> Start {
    let start = Start::default();
    Mock::given(method("POST"))
        .and(path("/v1/auth/discord/start"))
        .respond_with(start.clone())
        .mount(&env.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/token"))
        .and(wiremock::matchers::body_partial_json(
            json!({ "grant_type": "authorization_code" }),
        ))
        .respond_with(Exchange {
            start: start.clone(),
            expires_in,
        })
        .mount(&env.server)
        .await;
    start
}

fn fingerprint() -> String {
    root().public_key().fingerprint().to_string()
}

async fn login(env: &Env) -> Output {
    let helper = helper(env, None);
    env.run(
        &[
            "login",
            "--server",
            &env.uri(),
            "--fingerprint",
            &fingerprint(),
            "--authorize-with",
            helper.to_str().unwrap(),
        ],
        &[],
    )
    .await
}

fn session_files(env: &Env) -> Vec<PathBuf> {
    std::fs::read_dir(env.config())
        .map(|d| d.map(|e| e.unwrap().path()).collect())
        .unwrap_or_default()
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn login_uses_pkce_and_stores_the_session_privately_then_logout_revokes_it() {
    let env = Env::start().await;
    mount_login(&env, 900).await;
    let out = login(&env).await;
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Signed in to Test server as alice (owner)")
    );

    let files = session_files(&env);
    assert_eq!(files.len(), 1, "{files:?}");
    let stored = std::fs::read_to_string(&files[0]).unwrap();
    assert!(stored.contains(&token("vgr_", 'r')));
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&files[0]).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    Mock::given(method("POST"))
        .and(path("/v1/auth/logout"))
        .and(header(
            "authorization",
            format!("Bearer {}", token("vga_", 'a')).as_str(),
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&env.server)
        .await;
    let out = env.run(&["logout", "--server", &env.uri()], &[]).await;
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        session_files(&env).is_empty(),
        "the session file is removed"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn login_refuses_a_different_root_fingerprint_before_signing_in() {
    let env = Env::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/discord/start"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&env.server)
        .await;
    let other = SecretKey::from_seed(&[9; 32])
        .public_key()
        .fingerprint()
        .to_string();
    let out = env
        .run(
            &[
                "login",
                "--server",
                &env.uri(),
                "--fingerprint",
                &other,
                "--no-browser",
            ],
            &[],
        )
        .await;
    assert!(!out.status.success());
    assert!(text(&out).contains("refusing to sign in"), "{}", text(&out));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_pasted_link_from_another_sign_in_is_refused() {
    let env = Env::start().await;
    mount_login(&env, 900).await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/token"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&env.server)
        .await;
    let helper = helper(&env, Some("SomeoneElsesClientState"));
    let out = env
        .run(
            &[
                "login",
                "--server",
                &env.uri(),
                "--fingerprint",
                &fingerprint(),
                "--authorize-with",
                helper.to_str().unwrap(),
            ],
            &[],
        )
        .await;
    assert!(!out.status.success());
    assert!(text(&out).contains("another sign-in"), "{}", text(&out));
    assert!(session_files(&env).is_empty());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn an_expired_access_token_is_refreshed_and_the_new_pair_stored_first() {
    let env = Env::start().await;
    mount_login(&env, 0).await;
    let out = login(&env).await;
    assert!(out.status.success(), "{}", text(&out));

    Mock::given(method("POST"))
        .and(path("/v1/auth/token"))
        .and(body_json(
            json!({ "grant_type": "refresh_token", "refresh_token": token("vgr_", 'r') }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(tokens(
            &token("vga_", 'b'),
            &token("vgr_", 's'),
            900,
        )))
        .expect(1)
        .mount(&env.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/logout"))
        .and(header(
            "authorization",
            format!("Bearer {}", token("vga_", 'b')).as_str(),
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&env.server)
        .await;
    let out = env.run(&["logout", "--server", &env.uri()], &[]).await;
    assert!(out.status.success(), "{}", text(&out));
}

// ---------------------------------------------------------------------------
// trust publish

fn bearer() -> String {
    token("vga_", 'e')
}

async fn serve_bundle(env: &Env, signed: &SignedBundle) {
    Mock::given(method("GET"))
        .and(path("/v1/trust/bundle"))
        .respond_with(ResponseTemplate::new(200).set_body_json(signed))
        .mount(&env.server)
        .await;
}

async fn publish(env: &Env, signed: &SignedBundle, root_arg: &str) -> Output {
    let file = env.file("next.signed.json", &serde_json::to_vec(signed).unwrap());
    env.run(
        &[
            "trust",
            "publish",
            "--server",
            &env.uri(),
            "--signed",
            file.to_str().unwrap(),
            "--root",
            root_arg,
        ],
        &[("VGAMES_ACCESS_TOKEN", &bearer())],
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn trust_publish_uploads_a_newer_bundle_verified_under_the_root() {
    let env = Env::start().await;
    serve_bundle(&env, &bundle(1, &[old_key()], &[], &root(), SERVER_ID)).await;
    let next = bundle(2, &[new_key()], &[old_key()], &root(), SERVER_ID);
    Mock::given(method("POST"))
        .and(path("/v1/admin/trust/bundles"))
        .and(header(
            "authorization",
            format!("Bearer {}", bearer()).as_str(),
        ))
        .and(body_json(&next))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "version": 2 })))
        .expect(1)
        .mount(&env.server)
        .await;
    let out = publish(&env, &next, &root().public_key().to_base64()).await;
    assert!(out.status.success(), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("version 2"));
}

#[tokio::test(flavor = "multi_thread")]
async fn trust_publish_refuses_bad_bundles_without_uploading() {
    let attacker = SecretKey::from_seed(&[9; 32]);
    let root_b64 = root().public_key().to_base64();
    let cases = [
        (
            "signed by another key",
            bundle(2, &[new_key()], &[], &attacker, SERVER_ID),
            root_b64.clone(),
        ),
        (
            "for another server",
            bundle(
                2,
                &[new_key()],
                &[],
                &root(),
                "01920000-0000-7000-8000-00000000ffff",
            ),
            root_b64.clone(),
        ),
        (
            "not newer",
            bundle(1, &[new_key()], &[], &root(), SERVER_ID),
            root_b64.clone(),
        ),
        (
            "server advertises another root",
            bundle(2, &[new_key()], &[], &attacker, SERVER_ID),
            attacker.public_key().to_base64(),
        ),
    ];
    for (why, signed, root_arg) in cases {
        let env = Env::start().await;
        serve_bundle(&env, &bundle(1, &[old_key()], &[], &root(), SERVER_ID)).await;
        Mock::given(method("POST"))
            .and(path("/v1/admin/trust/bundles"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "version": 2 })))
            .expect(0)
            .mount(&env.server)
            .await;
        let out = publish(&env, &signed, &root_arg).await;
        assert!(!out.status.success(), "{why}: {}", text(&out));
    }
}

// ---------------------------------------------------------------------------
// trust re-sign

fn version(
    id: &str,
    platform: &str,
    sequence: i64,
    state: &str,
    current: bool,
    key: &SecretKey,
) -> Value {
    json!({ "id": id, "package_id": PACKAGE, "server_id": SERVER_ID, "platform": platform,
            "sequence": sequence, "version_label": format!("1.{sequence}"), "state": state,
            "is_current_release": current, "publisher_key_id": key.public_key().key_id(),
            "created_at": "2026-09-24T10:00:00Z", "finalized_at": "2026-09-24T11:00:00Z",
            "created_by": { "id": HOLDER, "username": "alice" } })
}

fn descriptor(version_id: &str, platform: &str, envelope: &Envelope) -> Value {
    json!({ "package_id": PACKAGE, "version_id": version_id, "platform": platform, "sequence": 12,
            "version_label": "1.4.2", "total_size": 1, "pack_count": 1,
            "manifest": { "url": "https://storage.example/m", "size": MANIFEST.len(),
                          "blake3": Digest::of(MANIFEST).to_string(), "expires_at": "2030-01-01T00:00:00Z" },
            "signature": envelope, "published_at": "2026-09-24T12:00:00Z" })
}

/// Captures the envelopes posted to `…/signature`.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<Value>>>);

impl Respond for Capture {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = request.body_json().unwrap();
        self.0.lock().unwrap().push(body);
        ResponseTemplate::new(200).set_body_json(version(
            CURRENT,
            "windows-x86_64",
            12,
            "published",
            true,
            &new_key(),
        ))
    }
}

struct ReSignFixture {
    env: Env,
    current: SignedBundle,
    key_file: PathBuf,
    previous: PathBuf,
    captured: Capture,
}

async fn re_sign_fixture() -> ReSignFixture {
    let env = Env::start().await;
    // v1 trusted the old key; v2 revokes it and trusts the new one.
    let previous = bundle(1, &[old_key()], &[], &root(), SERVER_ID);
    let current = bundle(2, &[new_key()], &[old_key()], &root(), SERVER_ID);
    serve_bundle(&env, &current).await;

    let old_envelope = Envelope::sign(&old_key(), Context::Manifest, MANIFEST);
    // Claims the old key id, but was not signed by it.
    let mut forged = Envelope::sign(
        &SecretKey::from_seed(&[9; 32]),
        Context::Manifest,
        b"evil manifest",
    );
    forged.key_id = old_key().public_key().key_id();

    Mock::given(method("GET"))
        .and(path("/v1/admin/packages"))
        .and(query_param("limit", "200"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [
            { "id": PACKAGE, "slug": "game", "title": "Game" } ] })))
        .mount(&env.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/admin/packages/{PACKAGE}/versions")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [
            version(CURRENT, "windows-x86_64", 12, "published", true, &old_key()),
            version(OLDER, "windows-x86_64", 11, "ready", false, &old_key()),
            version(FORGED, "linux-x86_64", 3, "published", true, &old_key()),
            version(OTHER_KEY, "macos-arm64", 2, "published", true, &new_key()),
        ] })))
        .mount(&env.server)
        .await;
    for (platform, id, envelope) in [
        ("windows-x86_64", CURRENT, &old_envelope),
        ("linux-x86_64", FORGED, &forged),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/v1/packages/{PACKAGE}/releases/{platform}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(descriptor(id, platform, envelope)),
            )
            .mount(&env.server)
            .await;
    }
    let captured = Capture::default();
    Mock::given(method("POST"))
        .and(path(format!("/v1/admin/versions/{CURRENT}/signature")))
        .respond_with(captured.clone())
        .mount(&env.server)
        .await;
    for id in [OLDER, FORGED, OTHER_KEY] {
        Mock::given(method("POST"))
            .and(path(format!("/v1/admin/versions/{id}/signature")))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&env.server)
            .await;
    }

    let created: vgames_core::Timestamp = "2026-09-25T09:00:00Z".parse().unwrap();
    let file = KeyFile::encrypt(
        &new_key(),
        KeyKind::Publisher,
        "alice@new-laptop",
        created,
        PASSPHRASE.as_bytes(),
    )
    .unwrap();
    let key_file = env.file("new.vgkey", &file.to_bytes());
    let previous = env.file("v1.signed.json", &serde_json::to_vec(&previous).unwrap());
    ReSignFixture {
        env,
        current,
        key_file,
        previous,
        captured,
    }
}

async fn re_sign(f: &ReSignFixture, extra: &[&str]) -> Output {
    let old_id = old_key().public_key().key_id().to_string();
    let root_b64 = root().public_key().to_base64();
    let uri = f.env.uri();
    let mut args = vec![
        "trust",
        "re-sign",
        "--server",
        &uri,
        "--from",
        &old_id,
        "--key",
        f.key_file.to_str().unwrap(),
        "--previous",
        f.previous.to_str().unwrap(),
        "--root",
        &root_b64,
        "--yes",
        "--passphrase-env",
        "VGAMES_TEST_PASS",
    ];
    args.extend_from_slice(extra);
    f.env
        .run(&args, &[("VGAMES_ACCESS_TOKEN", &bearer())])
        .await
}

fn expected() -> ExpectedRelease {
    ExpectedRelease {
        server_id: SERVER_ID.parse().unwrap(),
        package_id: PACKAGE.parse().unwrap(),
        version_id: CURRENT.parse().unwrap(),
        platform: vgames_core::manifest::Platform::WindowsX86_64,
        sequence: 12,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn re_sign_swaps_only_genuine_signatures_and_launchers_accept_the_result() {
    let f = re_sign_fixture().await;
    let out = re_sign(&f, &[]).await;
    assert!(out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("does not verify under the old key"),
        "{stdout}"
    );
    assert!(stdout.contains("only current releases"), "{stdout}");

    let posted = f.captured.0.lock().unwrap().clone();
    assert_eq!(posted.len(), 1, "exactly the genuine current release");
    let new_envelope =
        Envelope::parse(&serde_json::to_vec(&posted[0]["signature"]).unwrap()).unwrap();
    assert_eq!(new_envelope.key_id, new_key().public_key().key_id());
    assert_eq!(new_envelope.payload_blake3, Digest::of(MANIFEST));

    // Launcher side, with the new bundle: the old signature is refused, the new one accepted.
    let bytes = f.current.bundle_bytes().unwrap();
    let trust = verify_bundle(
        &bytes,
        &f.current.signature,
        &RootPin::new(root().public_key()),
        None,
        SERVER_ID.parse().unwrap(),
    )
    .unwrap()
    .state;
    let mode = VerifyMode::Install {
        now: "2026-09-25T10:00:00Z".parse().unwrap(),
        allow_older: false,
    };
    let old_envelope = Envelope::sign(&old_key(), Context::Manifest, MANIFEST);
    assert!(matches!(
        verify_manifest(&trust, &old_envelope, MANIFEST, &expected(), Some(12), mode),
        Err(VerifyError::RevokedKey(_))
    ));
    let verified =
        verify_manifest(&trust, &new_envelope, MANIFEST, &expected(), Some(12), mode).unwrap();
    assert_eq!(verified.key_id, new_key().public_key().key_id());
}

#[tokio::test(flavor = "multi_thread")]
async fn re_sign_dry_run_and_finalized_before_send_nothing() {
    let f = re_sign_fixture().await;
    let out = re_sign(&f, &["--dry-run"]).await;
    assert!(out.status.success(), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains(CURRENT));
    let out = re_sign(&f, &["--finalized-before", "2026-09-24T10:30:00Z"]).await;
    assert!(out.status.success(), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("finalized after --finalized-before"));
    assert!(f.captured.0.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn re_sign_refuses_a_new_key_the_bundle_does_not_trust() {
    let f = re_sign_fixture().await;
    let created: vgames_core::Timestamp = "2026-09-25T09:00:00Z".parse().unwrap();
    let stranger = KeyFile::encrypt(
        &SecretKey::from_seed(&[7; 32]),
        KeyKind::Publisher,
        "x",
        created,
        PASSPHRASE.as_bytes(),
    )
    .unwrap();
    let path_ = f.env.file("stranger.vgkey", &stranger.to_bytes());
    let old_id = old_key().public_key().key_id().to_string();
    let out = f
        .env
        .run(
            &[
                "trust",
                "re-sign",
                "--server",
                &f.env.uri(),
                "--from",
                &old_id,
                "--key",
                path_.to_str().unwrap(),
                "--root",
                &root().public_key().to_base64(),
                "--yes",
                "--passphrase-env",
                "VGAMES_TEST_PASS",
            ],
            &[("VGAMES_ACCESS_TOKEN", &bearer())],
        )
        .await;
    assert!(!out.status.success());
    assert!(
        text(&out).contains("not in the server's trust bundle"),
        "{}",
        text(&out)
    );
    assert!(f.captured.0.lock().unwrap().is_empty());
}
