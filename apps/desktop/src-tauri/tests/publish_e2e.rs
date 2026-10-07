//! INS-06: launcher admin publishing against the real API in process (PostgreSQL, fs storage),
//! through the launcher's own `Publisher` (the commands' code path).
//!
//! ```sh
//! VGAMES_TEST_DATABASE_URL=postgres://vgames:vgames-dev-only@localhost:5432/postgres \
//!   cargo test -p vgames-desktop --test publish_e2e -- --ignored --nocapture
//! ```
//!
//! `VGAMES_PUBLISH_E2E_GIB` (default 2; the nightly run uses 5) sizes the published tree.
//!
//! Covered: a wrong passphrase and an untrusted key refused before anything is sent; a failed
//! server verification; an upload cancelled part-way, the launcher restarted, the job resumed
//! (asking for the key again) to `ready`; release; yank.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

#[path = "support/install.rs"]
mod support;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use support::{Api, Desktop, PackageSpec, Published};
use uuid::Uuid;
use vgames_core::SecretKey;
use vgames_core::keyfile::{KeyFile, KeyKind};
use vgames_desktop_lib::catalog::types::Platform;
use vgames_desktop_lib::publishing::{
    KeyError, PackageCreate, PublishCommandError, PublishJob, PublishJobError, PublishLaunch,
    PublishPhase, PublishResume, PublishStart, UntrustedReason, VersionState,
};
use vgames_desktop_lib::servers::store::upsert_account;
use vgames_desktop_lib::servers::{Account, Role};
use vgames_transfer::testkit::package::{publisher_key, second_publisher_key};

const PASSPHRASE: &str = "e2e publisher passphrase";
const GIB: u64 = 1024 * 1024 * 1024;

fn size() -> u64 {
    let gib: f64 = std::env::var("VGAMES_PUBLISH_E2E_GIB")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|g: &f64| *g > 0.0 && *g <= 64.0)
        .unwrap_or(2.0);
    (gib * GIB as f64) as u64
}

fn key_file(dir: &Path, name: &str, key: &SecretKey) -> String {
    let file = KeyFile::encrypt(
        key,
        KeyKind::Publisher,
        "e2e@launcher",
        "2026-01-01T00:00:00Z".parse().unwrap(),
        PASSPHRASE.as_bytes(),
    )
    .unwrap();
    let path = dir.join(name);
    std::fs::write(&path, file.to_bytes()).unwrap();
    path.to_string_lossy().into_owned()
}

/// A launcher profile signed in as the API's owner (`first`: add the server too).
async fn launcher(data: &Path, api: &Api, first: bool) -> Desktop {
    let desktop = Desktop::open(data, api.server_id, &api.token).await;
    if first {
        desktop.add_server(&api.url).await;
    }
    let account = Account {
        user_id: api.owner_id,
        username: "e2e-owner".into(),
        display_name: None,
        role: Role::Owner,
    };
    let server_id = api.server_id;
    desktop
        .state
        .db
        .call(move |conn| upsert_account(conn, server_id, &account))
        .await
        .unwrap();
    desktop
}

fn start(package_id: Uuid, folder: &Path, key_path: &str, passphrase: &str) -> PublishStart {
    PublishStart {
        package_id,
        platform: Platform::LinuxX86_64,
        version_label: "1.0".into(),
        folder: folder.to_string_lossy().into_owned(),
        launch: Some(PublishLaunch {
            executable: support::GAME_SCRIPT.into(),
            args: vec![],
            working_dir: None,
        }),
        key_path: key_path.into(),
        passphrase: passphrase.into(),
    }
}

/// Waits (polling the job list, as the screen's first render does) until `done` holds.
async fn wait_for(
    desktop: &Desktop,
    id: Uuid,
    what: &str,
    limit: Duration,
    done: impl Fn(&PublishJob) -> bool,
) -> PublishJob {
    let deadline = Instant::now() + limit;
    loop {
        let job = desktop
            .state
            .publisher
            .jobs()
            .into_iter()
            .find(|j| j.id == id)
            .expect("the job is listed");
        if done(&job) {
            return job;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {job:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs PostgreSQL: set VGAMES_TEST_DATABASE_URL"]
async fn publish_e2e() {
    let size = size();
    let work = tempfile::tempdir().unwrap();
    let api = Api::start_paused().await;
    let data = work.path().join("launcher");
    let desktop = launcher(&data, &api, true).await;
    let server = api.server_id;
    let publisher = Arc::clone(&desktop.state.publisher);
    let key = key_file(work.path(), "publisher.vgkey", &publisher_key());
    let untrusted = key_file(work.path(), "other.vgkey", &second_publisher_key());

    let package = publisher
        .package_create(
            server,
            PackageCreate {
                title: "E2E Publish".into(),
                slug: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        publisher
            .package_create(
                server,
                PackageCreate {
                    title: "E2E Publish again".into(),
                    slug: Some(package.slug.clone()),
                },
            )
            .await,
        Err(PublishCommandError::SlugTaken)
    );

    // Key refusals come before anything is uploaded.
    let small = work.path().join("small");
    PackageSpec::small().generate(&small);
    assert_eq!(
        publisher
            .start(
                server,
                start(package.id, &small, &key, "not the passphrase")
            )
            .await,
        Err(PublishCommandError::Key {
            error: KeyError::WrongPassphrase
        })
    );
    assert_eq!(
        publisher
            .start(server, start(package.id, &small, &untrusted, PASSPHRASE))
            .await,
        Err(PublishCommandError::Key {
            error: KeyError::UntrustedKey {
                reason: UntrustedReason::Unknown
            }
        })
    );
    assert!(
        publisher
            .versions(server, package.id)
            .await
            .unwrap()
            .is_empty()
    );

    // A failed verification: a stored pack changes before the server verifies it.
    let broken = publisher
        .package_create(
            server,
            PackageCreate {
                title: "E2E Broken".into(),
                slug: None,
            },
        )
        .await
        .unwrap();
    let job = publisher
        .start(server, start(broken.id, &small, &key, PASSPHRASE))
        .await
        .unwrap();
    let verifying = wait_for(
        &desktop,
        job.id,
        "verifying",
        Duration::from_secs(120),
        |j| j.phase == PublishPhase::Verifying,
    )
    .await;
    assert_eq!(verifying.bytes_confirmed, verifying.bytes_total);
    api.flip_pack_byte(
        &Published {
            package_id: broken.id,
            version_id: verifying.version_id.unwrap(),
        },
        0,
        100,
    );
    api.start_workers();
    let failed = wait_for(
        &desktop,
        job.id,
        "a failed verification",
        Duration::from_secs(120),
        |j| j.phase == PublishPhase::Failed,
    )
    .await;
    assert!(
        matches!(
            failed.error,
            Some(PublishJobError::VerificationFailed { .. })
        ),
        "{failed:?}"
    );
    assert!(matches!(
        publisher.resume(job.id, None).await,
        Err(PublishCommandError::JobConflict {
            phase: PublishPhase::Failed
        })
    ));
    publisher.dismiss(job.id).await.unwrap();

    // The real tree: cancelled part-way, launcher restarted, resumed, verified, released.
    let source = work.path().join("source");
    let total = PackageSpec::sized(size).generate(&source);
    eprintln!("publishing {total} bytes");
    let began = Instant::now();
    let job = publisher
        .start(server, start(package.id, &source, &key, PASSPHRASE))
        .await
        .unwrap();
    // Progress counts stored pack bytes: random content is stored, so a little over the files.
    assert_eq!(
        job.bytes_total,
        job.packs.iter().map(|p| p.bytes_total).sum::<u64>()
    );
    assert!(job.bytes_total >= total, "{} < {total}", job.bytes_total);
    assert!(job.packs.len() > 1, "several packs: {}", job.packs.len());
    let part = wait_for(
        &desktop,
        job.id,
        "a tenth uploaded",
        Duration::from_secs(600),
        |j| j.bytes_confirmed >= total / 10,
    )
    .await;
    assert!(part.packs.iter().any(|p| p.bytes_confirmed > 0));
    let cancelled = publisher.cancel(job.id).await.unwrap();
    assert_eq!(cancelled.phase, PublishPhase::Cancelled, "{cancelled:?}");
    assert!(cancelled.resume_needs_key);
    let kept = cancelled.bytes_confirmed;
    assert!(
        kept > 0 && kept < cancelled.bytes_total,
        "{kept} of {}",
        cancelled.bytes_total
    );

    drop(publisher);
    drop(desktop);
    let desktop = launcher(&data, &api, false).await;
    let publisher = Arc::clone(&desktop.state.publisher);
    let listed = publisher.jobs();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].phase, PublishPhase::Cancelled);
    assert_eq!(
        publisher.resume(job.id, None).await,
        Err(PublishCommandError::KeyRequired)
    );
    publisher
        .resume(
            job.id,
            Some(PublishResume {
                key_path: key.clone(),
                passphrase: PASSPHRASE.into(),
            }),
        )
        .await
        .unwrap();
    let ready = wait_for(&desktop, job.id, "ready", Duration::from_secs(1800), |j| {
        !j.phase.is_running()
    })
    .await;
    assert_eq!(ready.phase, PublishPhase::Ready, "{ready:?}");
    assert_eq!(ready.bytes_confirmed, ready.bytes_total);
    assert!(
        ready
            .packs
            .iter()
            .all(|p| p.bytes_confirmed == p.bytes_total)
    );
    eprintln!(
        "{total} bytes published in {:.1} s (with the cancel and restart)",
        began.elapsed().as_secs_f64()
    );

    let version_id = ready.version_id.unwrap();
    let released = publisher.release(server, version_id).await.unwrap();
    assert_eq!(released.state, VersionState::Published);
    assert!(released.is_current_release);
    let versions = publisher.versions(server, package.id).await.unwrap();
    assert_eq!(versions.len(), 1, "the resume reused the version");
    assert_eq!(
        publisher.jobs()[0].phase,
        PublishPhase::Published,
        "the job follows the release"
    );
    let yanked = publisher
        .yank(server, version_id, "e2e: withdrawn".into())
        .await
        .unwrap();
    assert_eq!(yanked.state, VersionState::Yanked);

    api.stop().await;
}
