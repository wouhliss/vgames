//! `vgames publish` (A5-T06) against a mock of the vgames API and of the
//! storage behind its signed upload URLs. The mock's finalize checks what the
//! real server checks first: the manifest uploaded is the one signed, by the
//! publisher key, and it is valid.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tempfile::TempDir;
use vgames_core::keyfile::{KeyFile, KeyKind};
use vgames_core::sign::{Context, Envelope, SecretKey};
use vgames_core::trust::{SignedBundle, sign_bundle};
use vgames_core::{Digest, Timestamp};
use vgames_proto::versions::{FinalizeRequest, VersionState};
use wiremock::matchers::{header_exists, method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const SERVER_ID: &str = "01920000-0000-7000-8000-000000000000";
const HOLDER: &str = "0192aaaa-0000-7000-8000-000000000001";
const STRANGER: &str = "0192aaaa-0000-7000-8000-000000000002";
const PACKAGE: &str = "0192a6f0-1c2d-7e3f-8a9b-0c1d2e3f4a5b";
const VERSION: &str = "0192a6f1-aaaa-7bbb-8ccc-dddddddddddd";
const PASSPHRASE: &str = "correct horse battery staple 42";
const CREATED: &str = "2026-09-27T08:00:00Z";

fn root() -> SecretKey {
    SecretKey::from_seed(&[1; 32])
}
fn publisher() -> SecretKey {
    SecretKey::from_seed(&[2; 32])
}

fn bundle(revoked: bool) -> SignedBundle {
    let pk = publisher().public_key();
    let doc = json!({
        "format": "vgames.trust/1", "server_id": SERVER_ID, "version": 3,
        "issued_at": "2026-09-24T10:00:00Z", "expires_at": null,
        "root_key_id": root().public_key().key_id(),
        "publishers": [{ "key_id": pk.key_id(), "public_key": pk, "holder_user_id": HOLDER,
            "label": "alice", "not_before": "2026-09-24T00:00:00Z", "not_after": "2028-09-24T00:00:00Z" }],
        "revoked": if revoked {
            json!([{ "key_id": pk.key_id(), "revoked_at": "2026-09-25T08:00:00Z", "reason": "stolen" }])
        } else { json!([]) },
        "next_root": null
    });
    let bytes = serde_json::to_vec(&doc).unwrap();
    SignedBundle::new(&bytes, sign_bundle(&root(), &bytes))
}

fn user(id: &str) -> Value {
    json!({ "id": id, "username": "alice", "role": "owner", "created_at": "2026-09-24T10:00:00Z" })
}

fn package() -> Value {
    json!({ "id": PACKAGE, "slug": "my-game", "title": "My Game", "platforms": [],
            "updated_at": CREATED, "status": "draft", "field_sources": {}, "created_at": CREATED,
            "created_by": { "id": HOLDER, "username": "alice" } })
}

/// The storage and version state the mock server keeps between requests.
#[derive(Default)]
struct Backend {
    uri: String,
    state: Option<VersionState>,
    creates: u32,
    upload_sessions: u32,
    publishes: u32,
    /// How many finalize calls answer 503 before one succeeds.
    finalize_failures: u32,
    packs: BTreeMap<String, Vec<u8>>,
    /// Packs whose upload completed: a status query answers 200, as storage does.
    complete: BTreeMap<String, bool>,
    manifest: Option<Vec<u8>>,
    signed: Option<Vec<u8>>,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Backend>>);

impl Fake {
    fn version(&self) -> Value {
        let state = self
            .0
            .lock()
            .unwrap()
            .state
            .unwrap_or(VersionState::Uploading);
        json!({ "id": VERSION, "package_id": PACKAGE, "server_id": SERVER_ID,
                "platform": "linux-x86_64", "sequence": 7, "version_label": "1.4.0",
                "state": state.as_str(), "created_at": CREATED,
                "created_by": { "id": HOLDER, "username": "alice" } })
    }

    fn target(&self, url: String, verb: &str, headers: Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "url": url, "method": verb, "headers": headers, "expires_at": "2030-01-01T00:00:00Z" }))
    }
}

impl Respond for Fake {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let p = request.url.path().to_owned();
        let verb = request.method.as_str().to_owned();
        let uri = self.0.lock().unwrap().uri.clone();
        let v = format!("/v1/admin/versions/{VERSION}");
        match (verb.as_str(), p.as_str()) {
            ("POST", x) if x == format!("/v1/admin/packages/{PACKAGE}/versions") => {
                let body: Value = request.body_json().unwrap();
                assert_eq!(
                    body,
                    json!({ "platform": "linux-x86_64", "version_label": "1.4.0" })
                );
                let mut b = self.0.lock().unwrap();
                b.creates += 1;
                b.state.get_or_insert(VersionState::Uploading);
                drop(b);
                ResponseTemplate::new(201).set_body_json(self.version())
            }
            ("GET", x) if x == v => {
                let body = self.version();
                let mut b = self.0.lock().unwrap();
                // Verification finishes by the next poll.
                if b.state == Some(VersionState::Verifying) {
                    b.state = Some(VersionState::Ready);
                }
                ResponseTemplate::new(200).set_body_json(body)
            }
            ("POST", x) if x.starts_with(&format!("{v}/packs/")) => {
                let pack = x.split('/').nth(6).unwrap().to_owned();
                self.0.lock().unwrap().upload_sessions += 1;
                self.target(
                    format!("{uri}/storage/start/{pack}"),
                    "POST",
                    json!({ "Content-Type": "application/octet-stream", "x-goog-resumable": "start" }),
                )
            }
            ("POST", x) if x.starts_with("/storage/start/") => {
                let pack = x.trim_start_matches("/storage/start/");
                ResponseTemplate::new(201)
                    .insert_header("location", format!("{uri}/storage/session/{pack}"))
            }
            ("PUT", x) if x.starts_with("/storage/session/") => {
                let pack = x.trim_start_matches("/storage/session/").to_owned();
                let range = request
                    .headers
                    .get("content-range")
                    .unwrap()
                    .to_str()
                    .unwrap();
                let mut b = self.0.lock().unwrap();
                if b.complete.get(&pack).copied().unwrap_or(false) {
                    return ResponseTemplate::new(200);
                }
                let stored = b.packs.entry(pack.clone()).or_default();
                if range == "bytes */*" {
                    return match stored.len() {
                        0 => ResponseTemplate::new(308),
                        n => ResponseTemplate::new(308)
                            .insert_header("range", format!("bytes=0-{}", n - 1)),
                    };
                }
                let (span, total) = range.trim_start_matches("bytes ").split_once('/').unwrap();
                let start: usize = span.split_once('-').unwrap().0.parse().unwrap();
                stored.truncate(start);
                stored.extend_from_slice(&request.body);
                if stored.len() == total.parse::<usize>().unwrap() {
                    b.complete.insert(pack, true);
                    ResponseTemplate::new(200)
                } else {
                    ResponseTemplate::new(308)
                        .insert_header("range", format!("bytes=0-{}", stored.len() - 1))
                }
            }
            ("POST", x) if x == format!("{v}/manifest-upload") => self.target(
                format!("{uri}/storage/manifest"),
                "PUT",
                json!({ "Content-Type": "application/json" }),
            ),
            ("PUT", "/storage/manifest") => {
                self.0.lock().unwrap().manifest = Some(request.body.clone());
                ResponseTemplate::new(200)
            }
            ("POST", x) if x == format!("{v}/finalize") => {
                let mut b = self.0.lock().unwrap();
                if b.finalize_failures > 0 {
                    b.finalize_failures -= 1;
                    return ResponseTemplate::new(503).set_body_json(json!({
                        "type": "urn:vgames:problem:unavailable", "title": "Storage is unavailable",
                        "status": 503, "code": "storage_unavailable" }));
                }
                let req: FinalizeRequest = request.body_json().unwrap();
                let manifest = b
                    .manifest
                    .clone()
                    .expect("finalize before the manifest upload");
                assert_eq!(req.manifest_size, manifest.len() as i64);
                assert_eq!(req.manifest_blake3, Digest::of(&manifest).to_hex());
                let envelope: Envelope =
                    serde_json::from_value(serde_json::to_value(&req.signature).unwrap()).unwrap();
                envelope
                    .verify(&publisher().public_key(), Context::Manifest, &manifest)
                    .expect("the manifest is signed by the publisher key");
                vgames_core::manifest::parse_and_validate(&manifest).expect("a valid manifest");
                b.signed = Some(manifest);
                b.state = Some(VersionState::Verifying);
                drop(b);
                ResponseTemplate::new(202).set_body_json(self.version())
            }
            ("POST", x) if x == format!("{v}/publish") => {
                let mut b = self.0.lock().unwrap();
                assert_eq!(
                    b.state,
                    Some(VersionState::Ready),
                    "published before verification"
                );
                b.publishes += 1;
                b.state = Some(VersionState::Published);
                drop(b);
                ResponseTemplate::new(200).set_body_json(self.version())
            }
            other => {
                ResponseTemplate::new(501).set_body_string(format!("unexpected request {other:?}"))
            }
        }
    }
}

struct Env {
    server: MockServer,
    fake: Fake,
    dir: TempDir,
}

impl Env {
    async fn start(me: &str, revoked: bool) -> Self {
        let server = MockServer::start().await;
        let fake = Fake::default();
        fake.0.lock().unwrap().uri = server.uri();
        let info = json!({ "format": "vgames.server/1", "server_id": SERVER_ID, "name": "Test",
            "api_versions": ["v1"], "root_public_key": root().public_key().to_base64(),
            "root_key_fingerprint": root().public_key().fingerprint().to_string(), "features": [] });
        let me = json!({ "user": user(me), "session": { "id": VERSION, "kind": "desktop",
            "created_at": CREATED, "last_used_at": CREATED, "current": true } });
        for (p, body) in [
            ("/.well-known/vgames.json".to_owned(), info),
            ("/v1/me".to_owned(), me),
            (
                "/v1/trust/bundle".to_owned(),
                serde_json::to_value(bundle(revoked)).unwrap(),
            ),
            (format!("/v1/admin/packages/{PACKAGE}"), package()),
        ] {
            Mock::given(method("GET"))
                .and(path(p))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/v1/admin/packages"))
            .and(query_param("q", "my-game"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [package()] })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/admin/packages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [] })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/v1/admin/packages/{PACKAGE}/versions")))
            .and(header_exists("idempotency-key"))
            .respond_with(fake.clone())
            .mount(&server)
            .await;
        Mock::given(path_regex("^/(v1/admin/versions/|storage/)"))
            .respond_with(fake.clone())
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let created: Timestamp = "2026-09-25T09:00:00Z".parse().unwrap();
        let key = KeyFile::encrypt(
            &publisher(),
            KeyKind::Publisher,
            "alice",
            created,
            PASSPHRASE.as_bytes(),
        )
        .unwrap();
        std::fs::write(dir.path().join("publisher.vgkey"), key.to_bytes()).unwrap();
        let game = dir.path().join("game");
        std::fs::create_dir_all(game.join("bin")).unwrap();
        std::fs::create_dir_all(game.join("saves")).unwrap();
        std::fs::write(game.join("bin/game"), b"#!/bin/sh\necho hello\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(game.join("bin/game"), mode).unwrap();
        }
        std::fs::write(game.join("level1.dat"), vec![7u8; 300_000]).unwrap();
        std::fs::write(
            dir.path().join("launch.toml"),
            "[launch]\ndefault = \"game\"\n[[launch.targets]]\nid = \"game\"\nlabel = \"Play\"\nexecutable = \"bin/game\"\n",
        )
        .unwrap();
        Self { server, fake, dir }
    }

    fn game(&self) -> PathBuf {
        self.dir.path().join("game")
    }

    fn publish_dir(&self) -> PathBuf {
        self.dir.path().join("config/publish")
    }

    async fn publish(&self, package: &str, extra: &[&str]) -> Output {
        let key = self.dir.path().join("publisher.vgkey");
        let launch = self.dir.path().join("launch.toml");
        let mut args: Vec<String> = [
            "publish",
            self.game().to_str().unwrap(),
            "--server",
            &self.server.uri(),
            "--package",
            package,
            "--platform",
            "linux-x86_64",
            "--version-label",
            "1.4.0",
            "--key",
            key.to_str().unwrap(),
            "--execution",
            launch.to_str().unwrap(),
            "--root",
            &root().public_key().to_base64(),
            "--passphrase-env",
            "VGAMES_TEST_PASS",
        ]
        .map(str::to_owned)
        .to_vec();
        args.extend(extra.iter().map(|s| (*s).to_owned()));
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_vgames"));
        cmd.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("VGAMES_CONFIG_DIR", self.dir.path().join("config"))
            .env("VGAMES_TEST_PASS", PASSPHRASE)
            .env("VGAMES_ACCESS_TOKEN", format!("vga_{}", "e".repeat(43)))
            .args(args);
        tokio::task::spawn_blocking(move || cmd.output().unwrap())
            .await
            .unwrap()
    }

    fn backend(&self) -> std::sync::MutexGuard<'_, Backend> {
        self.fake.0.lock().unwrap()
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

fn files_in(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir).map_or_else(
        |_| Vec::new(),
        |entries| {
            entries
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        },
    )
}

#[tokio::test]
async fn publishes_a_folder_signed_by_the_publisher_key() {
    let env = Env::start(HOLDER, false).await;
    let out = env.publish("my-game", &["--publish", "--json"]).await;
    assert!(out.status.success(), "{}", text(&out));
    let version: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(version["state"], "published", "{}", text(&out));

    let b = env.backend();
    assert_eq!((b.creates, b.publishes), (1, 1));
    let manifest = vgames_core::manifest::parse_and_validate(b.signed.as_ref().unwrap()).unwrap();
    assert_eq!(manifest.version_id.to_string(), VERSION);
    assert_eq!(manifest.sequence, 7);
    let paths: Vec<_> = manifest.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["bin/game", "level1.dat"]);
    assert_eq!(
        manifest.files[0].executable,
        cfg!(unix),
        "the executable bit is kept"
    );
    assert!(!manifest.files[1].executable);
    assert_eq!(manifest.directories, ["saves"]);
    assert_eq!(
        manifest.launch.as_ref().unwrap().targets[0].executable,
        "bin/game"
    );
    // What was uploaded is what the signed manifest describes.
    assert_eq!(manifest.packs.len(), b.packs.len());
    for (i, pack) in manifest.packs.iter().enumerate() {
        let uploaded = &b.packs[&i.to_string()];
        assert_eq!(pack.size, uploaded.len() as u64);
        assert_eq!(pack.blake3, Digest::of(uploaded));
    }
    drop(b);
    let left = files_in(&env.publish_dir());
    assert!(
        left.is_empty(),
        "the resume state is removed once published: {left:?}"
    );
}

#[tokio::test]
async fn stops_at_ready_then_releases_without_uploading_again() {
    let env = Env::start(HOLDER, false).await;
    let out = env.publish(PACKAGE, &[]).await;
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("ready, not released"), "{}", text(&out));
    {
        let b = env.backend();
        assert_eq!(b.state, Some(VersionState::Ready));
        assert_eq!(b.publishes, 0);
    }
    assert!(
        !files_in(&env.publish_dir()).is_empty(),
        "kept for --publish"
    );

    let uploads = env.backend().upload_sessions;
    let out = env.publish(PACKAGE, &["--publish"]).await;
    assert!(out.status.success(), "{}", text(&out));
    let b = env.backend();
    assert_eq!(
        (b.creates, b.publishes),
        (1, 1),
        "the same version is released"
    );
    assert_eq!(b.upload_sessions, uploads, "nothing is uploaded again");
    assert_eq!(b.state, Some(VersionState::Published));
}

#[tokio::test]
async fn resumes_the_same_version_after_a_failed_step() {
    let env = Env::start(HOLDER, false).await;
    env.backend().finalize_failures = 1;
    let out = env.publish("my-game", &["--publish"]).await;
    assert!(!out.status.success(), "{}", text(&out));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("Storage is unavailable"),
        "the server's reason is shown: {err}"
    );
    assert!(err.contains("Run the same command again"), "{err}");

    let out = env.publish("my-game", &["--publish"]).await;
    assert!(out.status.success(), "{}", text(&out));
    let b = env.backend();
    assert_eq!(
        b.creates, 1,
        "the second run continues the first run's version"
    );
    assert_eq!(b.state, Some(VersionState::Published));
}

#[tokio::test]
async fn refuses_before_uploading_anything() {
    async fn refused(env: &Env, package: &str, expected: &str) {
        let out = env.publish(package, &["--publish"]).await;
        assert!(!out.status.success(), "{}", text(&out));
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(expected), "expected {expected:?} in: {err}");
        let b = env.backend();
        assert_eq!(
            (b.creates, b.upload_sessions),
            (0, 0),
            "nothing was created"
        );
    }

    let revoked = Env::start(HOLDER, true).await;
    refused(&revoked, "my-game", "is revoked").await;

    let stranger = Env::start(STRANGER, false).await;
    refused(&stranger, "my-game", "held by another account").await;

    let env = Env::start(HOLDER, false).await;
    refused(&env, "other-game", "no package with slug").await;
    refused(&env, "My Game!", "neither a package id nor a slug").await;

    std::fs::write(
        env.dir.path().join("launch.toml"),
        "[launch]\ndefault = \"game\"\n[[launch.targets]]\nid = \"game\"\nlabel = \"Play\"\nexecutable = \"bin/missing\"\n",
    )
    .unwrap();
    refused(&env, "my-game", "is not a file in the folder").await;

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc/passwd", env.game().join("passwd")).unwrap();
        refused(&env, "my-game", "passwd: a symbolic link").await;
    }
}
