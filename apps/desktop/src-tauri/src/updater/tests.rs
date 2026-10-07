//! Updater acceptance tests (A5-T09): a local update server, throwaway minisign
//! keys, and the real `tauri-plugin-updater` verification path, configured from
//! the launcher's own `tauri.conf.json` (only the public key is swapped).

use std::io::Cursor;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use tauri::test::MockRuntime;
use tauri_plugin_updater::UpdaterExt;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::policy::{self, PolicyError};
use super::remote::{self, Transport, UpdateError};

const TARGET: &str = "vgames-test";
const LOOPBACK: Transport = Transport {
    allow_loopback_http: true,
};
const HTTPS_ONLY: Transport = Transport {
    allow_loopback_http: false,
};

struct Keys {
    /// `plugins.updater.pubkey` form: base64 of the minisign public key file.
    pubkey: String,
    secret: minisign::SecretKey,
}

fn keys() -> Keys {
    let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let text = pair.pk.to_box().unwrap().to_string();
    Keys {
        pubkey: BASE64.encode(text),
        secret: pair.sk,
    }
}

/// A `.sig` as the release pipeline writes it: the trusted comment names the version.
fn sign(keys: &Keys, data: &[u8], version: Option<&str>) -> String {
    let trusted = match version {
        Some(v) => format!("timestamp:1790000000\tfile:vgames.AppImage\tversion:{v}"),
        None => "timestamp:1790000000\tfile:vgames.AppImage".to_owned(),
    };
    let signature = minisign::sign(None, &keys.secret, Cursor::new(data), Some(&trusted), None)
        .unwrap()
        .to_string();
    BASE64.encode(signature)
}

fn tauri_conf() -> Value {
    let raw = include_str!("../../tauri.conf.json");
    serde_json::from_str(raw).unwrap()
}

/// A launcher at `version` whose updater config is the shipped one, with `pubkey`.
fn launcher(pubkey: &str, version: &str) -> tauri::App<MockRuntime> {
    let mut updater = tauri_conf()["plugins"]["updater"].clone();
    updater["pubkey"] = json!(pubkey);
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context
        .config_mut()
        .plugins
        .0
        .insert("updater".into(), updater);
    context.package_info_mut().version = version.parse().unwrap();
    tauri::test::mock_builder()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .build(context)
        .unwrap()
}

fn artifact(version: &str) -> Vec<u8> {
    format!("vgames {version} test build\n")
        .repeat(64)
        .into_bytes()
}

struct Server {
    server: MockServer,
}

impl Server {
    async fn start() -> Self {
        Self {
            server: MockServer::start().await,
        }
    }

    fn url(&self, p: &str) -> Url {
        Url::parse(&format!("{}{p}", self.server.uri())).unwrap()
    }

    /// Serves `latest.json` announcing `version` at `download` with `signature`.
    async fn announce(&self, version: &str, download: &Url, signature: &str) {
        let latest = json!({
            "version": version,
            "notes": "- You can now pin favorite packages to the top of your library.",
            "pub_date": "2026-10-01T12:00:00Z",
            "platforms": { TARGET: { "url": download.as_str(), "signature": signature } }
        });
        Mock::given(method("GET"))
            .and(path("/latest.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(latest))
            .mount(&self.server)
            .await;
    }

    async fn serve(&self, p: &str, body: Vec<u8>) {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&self.server)
            .await;
    }
}

async fn find(
    app: &tauri::App<MockRuntime>,
    server: &Server,
    transport: Transport,
) -> Result<Option<tauri_plugin_updater::Update>, UpdateError> {
    let builder = app.updater_builder().target(TARGET);
    remote::find_update(builder, &server.url("/latest.json"), transport).await
}

/// Publishes version `new` signed by `signer` (for `signed_as`), serving `served` bytes.
async fn release(signer: &Keys, new: &str, signed_as: Option<&str>, served: Vec<u8>) -> Server {
    let server = Server::start().await;
    let signature = sign(signer, &artifact(new), signed_as);
    server.serve("/vgames.AppImage", served).await;
    server
        .announce(new, &server.url("/vgames.AppImage"), &signature)
        .await;
    server
}

#[test]
fn shipped_config_is_strict() {
    let conf = tauri_conf();
    let updater = &conf["plugins"]["updater"];
    assert_eq!(updater["requireSignedVersion"], json!(true));
    assert_eq!(updater["endpoints"], json!([remote::LATEST_URL]));
    for flag in [
        "dangerousInsecureTransportProtocol",
        "dangerousAcceptInvalidCerts",
        "dangerousAcceptInvalidHostnames",
        "allowDowngrades",
    ] {
        assert!(updater.get(flag).is_none(), "{flag} must stay unset");
    }
    for u in [remote::LATEST_URL, remote::CHANGELOG_URL] {
        policy::require_https(&Url::parse(u).unwrap(), false).unwrap();
    }
}

#[tokio::test]
async fn a_valid_update_downloads_verifies_and_installs() {
    let keys = keys();
    let server = release(&keys, "0.2.0", Some("0.2.0"), artifact("0.2.0")).await;
    let app = launcher(&keys.pubkey, "0.1.0");

    let update = find(&app, &server, LOOPBACK).await.unwrap().unwrap();
    assert_eq!(update.version, "0.2.0");
    let mut last = 0;
    let bytes = remote::download_verified(&update, |done, _| last = done)
        .await
        .unwrap();
    assert_eq!(bytes, artifact("0.2.0"));
    assert_eq!(last, bytes.len() as u64);

    // Linux (AppImage path): the running executable is replaced by the new build.
    #[cfg(target_os = "linux")]
    {
        let dir = tempfile::tempdir_in(std::env::temp_dir()).unwrap();
        let exe = dir.path().join("vgames.AppImage");
        std::fs::write(&exe, artifact("0.1.0")).unwrap();
        let builder = app.updater_builder().target(TARGET).executable_path(&exe);
        let update = remote::find_update(builder, &server.url("/latest.json"), LOOPBACK)
            .await
            .unwrap()
            .unwrap();
        let bytes = remote::download_verified(&update, |_, _| {}).await.unwrap();
        update.install(bytes).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), artifact("0.2.0"));
    }
}

#[tokio::test]
async fn a_tampered_artifact_is_rejected() {
    let keys = keys();
    let mut tampered = artifact("0.2.0");
    tampered[10] ^= 1;
    let server = release(&keys, "0.2.0", Some("0.2.0"), tampered).await;
    let app = launcher(&keys.pubkey, "0.1.0");

    let update = find(&app, &server, LOOPBACK).await.unwrap().unwrap();
    let error = remote::download_verified(&update, |_, _| {})
        .await
        .unwrap_err();
    assert!(error.is_integrity(), "{error:?}");
    assert!(matches!(
        error,
        UpdateError::Updater(tauri_plugin_updater::Error::Minisign(_))
    ));
}

#[tokio::test]
async fn an_update_signed_with_another_key_is_rejected() {
    let (trusted, attacker) = (keys(), keys());
    let server = release(&attacker, "0.2.0", Some("0.2.0"), artifact("0.2.0")).await;
    let app = launcher(&trusted.pubkey, "0.1.0");

    let update = find(&app, &server, LOOPBACK).await.unwrap().unwrap();
    let error = remote::download_verified(&update, |_, _| {})
        .await
        .unwrap_err();
    assert!(error.is_integrity(), "{error:?}");
}

#[tokio::test]
async fn an_older_or_equal_version_is_never_offered() {
    let keys = keys();
    for (installed, announced) in [
        ("0.3.0", "0.2.0"),
        ("0.2.0", "0.2.0"),
        ("1.0.0", "1.0.0-rc.1"),
    ] {
        let server = release(&keys, announced, Some(announced), artifact(announced)).await;
        let app = launcher(&keys.pubkey, installed);
        assert!(
            find(&app, &server, LOOPBACK).await.unwrap().is_none(),
            "{announced} offered over {installed}"
        );
    }
}

#[tokio::test]
async fn an_old_signed_build_replayed_as_a_new_version_is_rejected() {
    // The announcement is unsigned: a tampered latest.json pairs version 9.0.0 with
    // the genuine, validly signed 0.2.0 artifact to force a downgrade.
    let keys = keys();
    let server = Server::start().await;
    server.serve("/old.AppImage", artifact("0.2.0")).await;
    let old_signature = sign(&keys, &artifact("0.2.0"), Some("0.2.0"));
    server
        .announce("9.0.0", &server.url("/old.AppImage"), &old_signature)
        .await;
    let app = launcher(&keys.pubkey, "0.3.0");

    let update = find(&app, &server, LOOPBACK).await.unwrap().unwrap();
    let error = remote::download_verified(&update, |_, _| {})
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            UpdateError::Updater(tauri_plugin_updater::Error::SignedVersionMismatch { .. })
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_signature_without_a_version_is_rejected() {
    let keys = keys();
    let server = release(&keys, "0.2.0", None, artifact("0.2.0")).await;
    let app = launcher(&keys.pubkey, "0.1.0");

    let update = find(&app, &server, LOOPBACK).await.unwrap().unwrap();
    let error = remote::download_verified(&update, |_, _| {})
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            UpdateError::Updater(tauri_plugin_updater::Error::MissingSignedVersion)
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn plain_http_is_rejected() {
    let keys = keys();
    let server = release(&keys, "0.2.0", Some("0.2.0"), artifact("0.2.0")).await;
    let app = launcher(&keys.pubkey, "0.1.0");

    // Release builds: an http:// endpoint is refused before any request is made.
    let error = find(&app, &server, HTTPS_ONLY).await.err().unwrap();
    assert!(matches!(
        error,
        UpdateError::Policy(PolicyError::NotHttps(_))
    ));
    assert!(server.server.received_requests().await.unwrap().is_empty());

    // Even where loopback HTTP is allowed, a download URL on another host over
    // plain HTTP is refused.
    let server = Server::start().await;
    let signature = sign(&keys, &artifact("0.2.0"), Some("0.2.0"));
    let insecure = Url::parse("http://updates.example/vgames.AppImage").unwrap();
    server.announce("0.2.0", &insecure, &signature).await;
    let error = find(&app, &server, LOOPBACK).await.err().unwrap();
    assert!(matches!(
        error,
        UpdateError::Policy(PolicyError::NotHttps(_))
    ));

    // The changelog too.
    let error = remote::fetch_changelog(&server.url("/changelog-user.json"), HTTPS_ONLY)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        UpdateError::Policy(PolicyError::NotHttps(_))
    ));
}

#[tokio::test]
async fn redirects_to_plain_http_are_not_followed() {
    let server = Server::start().await;
    Mock::given(method("GET"))
        .and(path("/changelog-user.json"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "http://updates.example/changelog-user.json"),
        )
        .mount(&server.server)
        .await;
    let error = remote::fetch_changelog(&server.url("/changelog-user.json"), LOOPBACK)
        .await
        .unwrap_err();
    assert!(
        matches!(error, UpdateError::Http(ref e) if e.is_redirect()),
        "{error:?}"
    );
}

/// A raw HTTP server that announces `declared` bytes, sends 64 KiB, then stalls
/// (hyper-based mocks refuse to send a length they do not deliver).
async fn lying_server(declared: u64) -> Url {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = [0u8; 4096];
                let _ = socket.read(&mut request).await;
                let head = format!("HTTP/1.1 200 OK\r\ncontent-length: {declared}\r\n\r\n");
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&[0u8; 64 * 1024]).await;
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            });
        }
    });
    Url::parse(&format!("http://{address}/huge.AppImage")).unwrap()
}

#[tokio::test]
async fn an_oversized_update_is_abandoned() {
    let keys = keys();
    let server = Server::start().await;
    let signature = sign(&keys, &artifact("0.2.0"), Some("0.2.0"));
    let huge = lying_server(remote::MAX_UPDATE_BYTES + 1).await;
    server.announce("0.2.0", &huge, &signature).await;
    let app = launcher(&keys.pubkey, "0.1.0");

    let update = find(&app, &server, LOOPBACK).await.unwrap().unwrap();
    // Stops at the first chunk instead of waiting for (or buffering) 512 MiB.
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        remote::download_verified(&update, |_, _| {}),
    )
    .await
    .expect("the download must stop at once")
    .unwrap_err();
    assert!(matches!(error, UpdateError::TooLarge), "{error:?}");
}

fn changelog_doc() -> Value {
    json!({
        "format": "vgames.changelog/1",
        "releases": [
            { "version": "0.2.0", "date": "2026-10-01", "entries": [
                { "type": "added", "text": "You can now pin favorite packages to the top of your library." } ] }
        ]
    })
}

async fn serve_changelog(body: ResponseTemplate) -> Server {
    let server = Server::start().await;
    Mock::given(method("GET"))
        .and(path("/changelog-user.json"))
        .respond_with(body)
        .mount(&server.server)
        .await;
    server
}

#[tokio::test]
async fn the_changelog_is_fetched_and_validated() {
    let server = serve_changelog(ResponseTemplate::new(200).set_body_json(changelog_doc())).await;
    let releases = remote::fetch_changelog(&server.url("/changelog-user.json"), LOOPBACK)
        .await
        .unwrap();
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].entries.len(), 1);
}

#[tokio::test]
async fn an_oversized_changelog_is_rejected() {
    // Declared too large.
    let mut padded = serde_json::to_vec(&changelog_doc()).unwrap();
    padded.resize(policy::MAX_CHANGELOG_BYTES + 1, b' ');
    let server = serve_changelog(ResponseTemplate::new(200).set_body_bytes(padded)).await;
    let error = remote::fetch_changelog(&server.url("/changelog-user.json"), LOOPBACK)
        .await
        .unwrap_err();
    assert!(
        matches!(error, UpdateError::Policy(PolicyError::ChangelogTooLarge)),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_malformed_changelog_is_rejected_and_the_notes_are_used() {
    let bad = json!({ "format": "vgames.changelog/1", "releases": [
        { "version": "0.2.0", "date": "2026-10-01", "entries": [
            { "type": "added", "text": "<script>alert(1)</script>", "html": true } ] } ] });
    let server = serve_changelog(ResponseTemplate::new(200).set_body_json(bad)).await;
    let error = remote::fetch_changelog(&server.url("/changelog-user.json"), LOOPBACK)
        .await
        .unwrap_err();
    assert!(
        matches!(error, UpdateError::Policy(PolicyError::ChangelogInvalid(_))),
        "{error:?}"
    );

    // What the dialog then shows: the latest.json notes, as plain text.
    let whats_new = policy::whats_new(
        None,
        Some("- Downloads now resume after your computer restarts."),
        &"0.1.0".parse().unwrap(),
        &"0.2.0".parse().unwrap(),
    );
    assert!(whats_new.from_latest_notes);
    assert_eq!(
        whats_new.releases[0].entries[0].text,
        "Downloads now resume after your computer restarts."
    );
}

/// INS-03: `updater_install` first pauses every download at a checkpoint and
/// only then waits for the launcher to be idle; without that pause an active
/// download keeps it waiting.
#[tokio::test]
async fn install_waits_for_downloads_to_pause_at_a_checkpoint() {
    use std::sync::atomic::{AtomicBool, Ordering};

    use tokio_util::sync::CancellationToken;

    use super::{DownloadCheckpoint, Updater, quiesce};

    struct Fake {
        updater: Updater,
        pauses: bool,
        paused: AtomicBool,
    }

    impl DownloadCheckpoint for Fake {
        async fn pause_all_at_checkpoint(&self) {
            if self.pauses {
                // What the worker's pause produces on the bus.
                self.updater.with(|i| {
                    i.activity
                        .install_progress((uuid::Uuid::nil(), uuid::Uuid::nil()), true)
                });
                self.updater.activity_changed.notify_waiters();
                self.paused.store(true, Ordering::SeqCst);
            }
        }

        fn release_checkpoint(&self) {}
    }

    let updater = Updater::new();
    updater.with(|i| {
        i.activity
            .install_progress((uuid::Uuid::nil(), uuid::Uuid::nil()), false)
    });
    let shutdown = CancellationToken::new();

    let ignores = Fake {
        updater: updater.clone(),
        pauses: false,
        paused: AtomicBool::new(false),
    };
    let waiting = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        quiesce(&ignores, &updater, &shutdown),
    )
    .await;
    assert!(waiting.is_err(), "an active download blocks the install");

    let pauses = Fake {
        updater: updater.clone(),
        pauses: true,
        paused: AtomicBool::new(false),
    };
    assert!(quiesce(&pauses, &updater, &shutdown).await.is_some());
    assert!(pauses.paused.load(Ordering::SeqCst));
}

#[tokio::test]
async fn checkpoint_releases_on_shutdown_and_aborted_install() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Queue(AtomicUsize);
    impl super::DownloadCheckpoint for Queue {
        async fn pause_all_at_checkpoint(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<()>().await;
        }
        fn release_checkpoint(&self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let queue = std::sync::Arc::new(Queue(AtomicUsize::new(0)));
    let updater = super::Updater::new();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let cancelled = shutdown.clone();
    let worker_queue = queue.clone();
    let worker = tokio::spawn(async move {
        assert!(
            super::quiesce(worker_queue.as_ref(), &updater, &cancelled)
                .await
                .is_none()
        );
    });
    while queue.0.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    shutdown.cancel();
    worker.await.unwrap();
    assert_eq!(queue.0.load(Ordering::SeqCst), 0);

    let worker_queue = queue.clone();
    let worker = tokio::spawn(async move {
        let updater = super::Updater::new();
        let shutdown = tokio_util::sync::CancellationToken::new();
        let _checkpoint = super::quiesce(worker_queue.as_ref(), &updater, &shutdown).await;
    });
    while queue.0.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    assert_eq!(queue.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn checkpoint_releases_after_an_install_error_or_missing_update() {
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Queue(AtomicBool);
    impl super::DownloadCheckpoint for Queue {
        async fn pause_all_at_checkpoint(&self) {
            self.0.store(true, Ordering::SeqCst);
        }
        fn release_checkpoint(&self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    async fn attempt(queue: &Queue, pending: bool) -> Result<(), &'static str> {
        let updater = super::Updater::new();
        let shutdown = tokio_util::sync::CancellationToken::new();
        let _checkpoint = super::quiesce(queue, &updater, &shutdown).await.unwrap();
        assert!(queue.0.load(Ordering::SeqCst));
        if !pending {
            return Err("no update");
        }
        Err("verification failed")
    }
    let queue = Queue(AtomicBool::new(false));
    for pending in [false, true] {
        assert!(attempt(&queue, pending).await.is_err());
        assert!(!queue.0.load(Ordering::SeqCst));
    }
}
