//! INS-06 checks that need no upload: the admin role, every key refusal before anything is sent,
//! the typed input errors, release and yank against the version's state, and jobs surviving a
//! restart. Uploading, cancel-then-resume and a failed verification run against the real API in
//! `tests/publish_e2e.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::sync::Arc;

use serde_json::json;
use time::OffsetDateTime;
use url::Url;
use vgames_core::keyfile::{KeyFile, KeyKind};
use vgames_proto::auth::UserPublic;
use vgames_proto::packages::Platform as WirePlatform;
use vgames_transfer::testkit::package::{
    ADMIN_ID, publisher_key, second_publisher_key, trust_state, trust_state_with,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::api::Session;
use crate::catalog::types::Platform;
use crate::secrets::{MemoryVault, VaultHandle};

const PASSPHRASE: &str = "correct horse battery staple";

struct Stub {
    client: ApiClient,
    role: Role,
    user_id: Uuid,
    trust: Option<Arc<TrustState>>,
}

impl Connections for Stub {
    async fn client(&self, _: Uuid) -> Result<ApiClient, PublishCommandError> {
        Ok(self.client.clone())
    }
    async fn account(&self, _: Uuid) -> Result<Account, PublishCommandError> {
        Ok(Account {
            user_id: self.user_id,
            username: "admin".to_owned(),
            display_name: None,
            role: self.role,
        })
    }
    async fn trust(&self, _: Uuid) -> Result<Option<Arc<TrustState>>, PublishCommandError> {
        Ok(self.trust.clone())
    }
}

struct Harness {
    mock: MockServer,
    publisher: Arc<Publisher<Stub>>,
    work: tempfile::TempDir,
    data: PathBuf,
}

fn server_id() -> Uuid {
    vgames_transfer::testkit::package::SERVER_ID
}

async fn client(base: Url) -> ApiClient {
    let session = Arc::new(Session::new(
        server_id(),
        base.clone(),
        VaultHandle(Arc::new(MemoryVault::default())),
        Arc::new(|_| {}),
    ));
    session
        .store(
            &serde_json::from_value(json!({
                "access_token": "access-1",
                "refresh_token": "refresh-1",
                "token_type": "Bearer",
                "expires_in": 900,
                "user": {
                    "id": ADMIN_ID,
                    "username": "admin",
                    "role": "admin",
                    "created_at": "2026-09-24T10:00:00Z"
                }
            }))
            .unwrap(),
        )
        .await;
    ApiClient::new(crate::api::http_client().unwrap(), base, session)
}

impl Harness {
    async fn new(role: Role, user_id: Uuid, trust: Option<TrustState>) -> Self {
        let mock = MockServer::start().await;
        let work = tempfile::tempdir().unwrap();
        let data = work.path().join("publishing");
        let publisher = Arc::new(Publisher::open(
            Arc::new(Stub {
                client: client(mock.uri().parse().unwrap()).await,
                role,
                user_id,
                trust: trust.map(Arc::new),
            }),
            data.clone(),
            EventBus::new(),
            PublishOptions::default(),
        ));
        Self {
            mock,
            publisher,
            work,
            data,
        }
    }

    async fn admin() -> Self {
        Self::new(Role::Admin, ADMIN_ID, Some(trust_state())).await
    }

    fn folder(&self) -> PathBuf {
        let folder = self.work.path().join("build");
        std::fs::create_dir_all(folder.join("bin")).unwrap();
        std::fs::write(folder.join("bin/game.sh"), b"#!/bin/sh\necho hi\n").unwrap();
        std::fs::write(folder.join("data.pak"), vec![5u8; 10_000]).unwrap();
        folder
    }

    fn key_file(&self, key: &SecretKey, kind: KeyKind) -> String {
        let file = KeyFile::encrypt(
            key,
            kind,
            "test@launcher",
            "2026-01-01T00:00:00Z".parse().unwrap(),
            PASSPHRASE.as_bytes(),
        )
        .unwrap();
        let path = self.work.path().join(format!("{}.vgkey", Uuid::now_v7()));
        std::fs::write(&path, file.to_bytes()).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn start(&self, key_path: String, passphrase: &str) -> PublishStart {
        PublishStart {
            package_id: Uuid::from_u128(7),
            platform: Platform::LinuxX86_64,
            version_label: "1.0".to_owned(),
            folder: self.folder().to_string_lossy().into_owned(),
            launch: Some(PublishLaunch {
                executable: "bin/game.sh".to_owned(),
                args: vec![],
                working_dir: None,
            }),
            key_path,
            passphrase: passphrase.to_owned(),
        }
    }

    async fn requests(&self) -> usize {
        self.mock.received_requests().await.unwrap().len()
    }
}

fn version(state: WireState) -> Version {
    Version {
        id: Uuid::from_u128(9),
        package_id: Uuid::from_u128(7),
        server_id: server_id(),
        platform: WirePlatform::LinuxX86_64,
        sequence: 1,
        version_label: "1.0".to_owned(),
        state,
        is_current_release: None,
        failure_reason: None,
        total_size: None,
        file_count: None,
        chunk_count: None,
        pack_count: None,
        publisher_key_id: None,
        signature: None,
        verify_progress: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
        created_by: UserPublic {
            id: ADMIN_ID,
            username: "admin".to_owned(),
            display_name: None,
            avatar_url: None,
        },
        finalized_at: None,
        verified_at: None,
        published_at: None,
        yanked_at: None,
    }
}

#[tokio::test]
async fn accounts_that_are_not_admins_are_refused_before_any_request() {
    let h = Harness::new(Role::User, ADMIN_ID, Some(trust_state())).await;
    assert_eq!(
        h.publisher.packages(server_id(), None, None).await,
        Err(PublishCommandError::Forbidden)
    );
    let key = h.key_file(&publisher_key(), KeyKind::Publisher);
    assert_eq!(
        h.publisher
            .start(server_id(), h.start(key, PASSPHRASE))
            .await,
        Err(PublishCommandError::Forbidden)
    );
    assert_eq!(h.requests().await, 0);
}

#[tokio::test]
async fn every_key_refusal_comes_before_anything_is_sent() {
    let untrusted = |reason| {
        Err(PublishCommandError::Key {
            error: KeyError::UntrustedKey { reason },
        })
    };
    let h = Harness::admin().await;
    let good = h.key_file(&publisher_key(), KeyKind::Publisher);

    let wrong = h
        .publisher
        .start(server_id(), h.start(good.clone(), "nope"))
        .await;
    assert_eq!(
        wrong,
        Err(PublishCommandError::Key {
            error: KeyError::WrongPassphrase
        })
    );
    let not_a_key = h.work.path().join("notes.vgkey");
    std::fs::write(&not_a_key, b"hello").unwrap();
    let root_kind = h.key_file(&publisher_key(), KeyKind::Root);
    for path in [not_a_key.to_string_lossy().into_owned(), root_kind] {
        assert_eq!(
            h.publisher
                .start(server_id(), h.start(path, PASSPHRASE))
                .await,
            Err(PublishCommandError::Key {
                error: KeyError::InvalidKeyFile
            })
        );
    }
    let other_key = h.key_file(&second_publisher_key(), KeyKind::Publisher);
    assert_eq!(
        h.publisher
            .start(server_id(), h.start(other_key, PASSPHRASE))
            .await,
        untrusted(UntrustedReason::Unknown)
    );
    assert_eq!(h.requests().await, 0);

    let revoked = Harness::new(Role::Admin, ADMIN_ID, Some(trust_state_with(2, true))).await;
    let key = revoked.key_file(&publisher_key(), KeyKind::Publisher);
    assert_eq!(
        revoked
            .publisher
            .start(server_id(), revoked.start(key, PASSPHRASE))
            .await,
        untrusted(UntrustedReason::Revoked)
    );
    let someone_else = Harness::new(Role::Owner, Uuid::from_u128(42), Some(trust_state())).await;
    let key = someone_else.key_file(&publisher_key(), KeyKind::Publisher);
    assert_eq!(
        someone_else
            .publisher
            .start(server_id(), someone_else.start(key, PASSPHRASE))
            .await,
        untrusted(UntrustedReason::OtherHolder)
    );
    let no_bundle = Harness::new(Role::Admin, ADMIN_ID, None).await;
    let key = no_bundle.key_file(&publisher_key(), KeyKind::Publisher);
    assert_eq!(
        no_bundle
            .publisher
            .start(server_id(), no_bundle.start(key, PASSPHRASE))
            .await,
        untrusted(UntrustedReason::NoBundle)
    );
    for h in [revoked, someone_else, no_bundle] {
        assert_eq!(h.requests().await, 0);
    }
}

#[test]
fn a_key_outside_its_validity_window_is_refused() {
    let trust = trust_state();
    let key = publisher_key();
    let at = |s: &str| s.parse().unwrap();
    assert_eq!(
        check_trust(Some(&trust), &key, ADMIN_ID, at("2025-06-01T00:00:00Z")),
        Err(KeyError::UntrustedKey {
            reason: UntrustedReason::NotValidNow
        })
    );
    assert_eq!(
        check_trust(Some(&trust), &key, ADMIN_ID, at("2036-01-01T00:00:00Z")),
        Err(KeyError::UntrustedKey {
            reason: UntrustedReason::NotValidNow
        })
    );
    assert_eq!(
        check_trust(Some(&trust), &key, ADMIN_ID, at("2030-01-01T00:00:00Z")),
        Ok(())
    );
}

#[tokio::test]
async fn bad_inputs_are_typed_and_send_nothing() {
    let h = Harness::admin().await;
    let key = h.key_file(&publisher_key(), KeyKind::Publisher);

    let mut start = h.start(key.clone(), PASSPHRASE);
    start.version_label = " ".to_owned();
    assert_eq!(
        h.publisher.start(server_id(), start).await,
        Err(PublishCommandError::InvalidLabel)
    );
    let mut start = h.start(key.clone(), PASSPHRASE);
    start.launch = Some(PublishLaunch {
        executable: "bin/missing.exe".to_owned(),
        args: vec![],
        working_dir: None,
    });
    assert_eq!(
        h.publisher.start(server_id(), start).await,
        Err(PublishCommandError::InvalidLaunch)
    );
    let start = h.start(key, PASSPHRASE);
    std::fs::write(Path::new(&start.folder).join("AUX.dat"), b"x").unwrap();
    assert_eq!(
        h.publisher.start(server_id(), start).await,
        Err(PublishCommandError::InvalidPaths { count: 1 })
    );
    assert_eq!(h.requests().await, 0);
    assert!(h.publisher.jobs().is_empty());
}

#[tokio::test]
async fn release_and_yank_check_the_version_state_first() {
    let h = Harness::admin().await;
    Mock::given(method("GET"))
        .and(path(
            "/v1/admin/versions/00000000-0000-0000-0000-000000000009",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(version(WireState::Uploading)))
        .mount(&h.mock)
        .await;
    let id = Uuid::from_u128(9);
    assert_eq!(
        h.publisher.release(server_id(), id).await,
        Err(PublishCommandError::VersionConflict {
            state: VersionState::Uploading
        })
    );
    assert_eq!(
        h.publisher
            .yank(server_id(), id, "broken save files".to_owned())
            .await,
        Err(PublishCommandError::VersionConflict {
            state: VersionState::Uploading
        })
    );
    assert!(matches!(
        h.publisher.yank(server_id(), id, "no".to_owned()).await,
        Err(PublishCommandError::InvalidField { .. })
    ));
    let posts = h
        .mock
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "POST")
        .count();
    assert_eq!(posts, 0);
}

#[tokio::test]
async fn release_publishes_a_ready_version() {
    let h = Harness::admin().await;
    Mock::given(method("GET"))
        .and(path(
            "/v1/admin/versions/00000000-0000-0000-0000-000000000009",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(version(WireState::Ready)))
        .mount(&h.mock)
        .await;
    let mut published = version(WireState::Published);
    published.is_current_release = Some(true);
    Mock::given(method("POST"))
        .and(path(
            "/v1/admin/versions/00000000-0000-0000-0000-000000000009/publish",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(published))
        .expect(1)
        .mount(&h.mock)
        .await;
    let released = h
        .publisher
        .release(server_id(), Uuid::from_u128(9))
        .await
        .unwrap();
    assert_eq!(released.state, VersionState::Published);
    assert!(released.is_current_release);
}

#[tokio::test]
async fn a_job_interrupted_by_a_restart_comes_back_cancelled_and_dismiss_aborts_its_version() {
    let h = Harness::admin().await;
    let id = Uuid::now_v7();
    let record = Record {
        format: RECORD_FORMAT,
        job: PublishJob {
            id,
            server_id: server_id(),
            package_id: Uuid::from_u128(7),
            package_title: "Game".to_owned(),
            platform: Platform::LinuxX86_64,
            version_label: "1.0".to_owned(),
            version_id: Some(Uuid::from_u128(9)),
            phase: PublishPhase::Uploading,
            bytes_confirmed: 10,
            bytes_total: 100,
            bytes_per_second: 5.0,
            packs: vec![],
            verification: None,
            error: None,
            resume_needs_key: true,
        },
        folder: h.folder(),
        launch: None,
        idempotency_key: Uuid::now_v7(),
        version: Some(version(WireState::Uploading)),
    };
    std::fs::create_dir_all(h.data.join(id.to_string())).unwrap();
    std::fs::write(
        h.data.join(id.to_string()).join("job.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();

    let reopened = Publisher::open(
        Arc::new(Stub {
            client: client(h.mock.uri().parse().unwrap()).await,
            role: Role::Admin,
            user_id: ADMIN_ID,
            trust: Some(Arc::new(trust_state())),
        }),
        h.data.clone(),
        EventBus::new(),
        PublishOptions::default(),
    );
    let jobs = reopened.jobs();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].phase, PublishPhase::Cancelled);
    assert_eq!(jobs[0].bytes_per_second, 0.0);
    assert!(jobs[0].resume_needs_key);
    let reopened = Arc::new(reopened);
    assert_eq!(
        reopened.resume(id, None).await,
        Err(PublishCommandError::KeyRequired)
    );

    Mock::given(method("DELETE"))
        .and(path(
            "/v1/admin/versions/00000000-0000-0000-0000-000000000009",
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&h.mock)
        .await;
    reopened.dismiss(id).await.unwrap();
    assert!(reopened.jobs().is_empty());
    assert!(!h.data.join(id.to_string()).exists());
    assert_eq!(
        reopened.cancel(id).await,
        Err(PublishCommandError::NotFound)
    );
}

#[test]
fn the_start_request_never_prints_the_passphrase() {
    let start = PublishStart {
        package_id: Uuid::from_u128(7),
        platform: Platform::LinuxX86_64,
        version_label: "1.0".to_owned(),
        folder: "/tmp/x".to_owned(),
        launch: None,
        key_path: "/keys/k.vgkey".to_owned(),
        passphrase: "hunter2-secret".to_owned(),
    };
    let printed = format!("{start:?}");
    assert!(!printed.contains("hunter2"));
    assert!(!printed.contains("k.vgkey"));
}
