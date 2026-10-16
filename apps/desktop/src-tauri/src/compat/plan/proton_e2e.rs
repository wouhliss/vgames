//! The D3D11 test game through real umu-launcher and UMU-Proton (GAME-05),
//! started by `Launcher::launch` like `game_launch`. Ignored by default; the hosted
//! `compat-e2e.yml` job runs it on `ubuntu-24.04` (Xvfb, lavapipe) with:
//!
//! - `VGAMES_PROTON_E2E_RUNTIMES`: a JSON array of runtime-catalog entries pinning
//!   the real upstream archives (`scripts/ci/compat-runtimes.sh` writes it);
//! - `VGAMES_PROTON_E2E_GAME`: the Windows test game (`vgames-testapp-d3d11.exe`);
//! - `VGAMES_PROTON_E2E_DATA` (optional): a launcher data directory kept between runs,
//!   so the D3D12 run reuses the runtimes the D3D11 run installed;
//! - `VGAMES_PROTON_E2E_EXPECT` (optional): `ok` (default) or `refused-before-extraction`
//!   (the job flips one pinned hash to prove a tampered archive never runs);
//! - `VGAMES_PROTON_E2E_LOG_DIR` (optional): the game's target sets `PROTON_LOG=1`
//!   and `PROTON_LOG_DIR` (and DXVK's log) there, so a failed run leaves the logs
//!   for the job;
//! - `VGAMES_PROTON_E2E_OVERLAY` (optional): the built `libvgames_overlay.so`. The run then
//!   registers the Vulkan layer and starts a real overlay broker, like a launch with the
//!   overlay on, and asserts the layer connected from inside the Proton game (GAME-08).
//!
//! The catalog is signed here with a key generated for the run and served locally;
//! the runtimes themselves come from GitHub, verified by size and SHA-256.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;
use vgames_pack::manifest::{Execution, Launch, LaunchTarget};
use vgames_pack::{Compression, PACK_SIZE};
use vgames_transfer::install::{
    InstallRecord, InstallState, MANIFEST_FILE, META_DIR, SIGNATURE_FILE, write_record,
};
use vgames_transfer::testkit::{Content, FileSpec, Identity, TestPackage, write_tree};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::compat::catalog::{CatalogSource, http_client};
use crate::db::{Db, rusqlite};
use crate::events::{AppEvent, EventBus};
use crate::launch::orchestrate::{Launcher, TrustSource};
use crate::launch::{GameSessions, TargetChoice};

struct Trust(Arc<vgames_core::trust::TrustState>);

impl TrustSource for Trust {
    async fn trust(
        &self,
        _: uuid::Uuid,
    ) -> Result<Option<Arc<vgames_core::trust::TrustState>>, crate::db::DbError> {
        Ok(Some(Arc::clone(&self.0)))
    }
}

/// No profile is published: Proton runs with the defaults.
struct NoProfiles;

impl ProfileSource for NoProfiles {
    async fn fetch(
        &self,
        _: uuid::Uuid,
        _: uuid::Uuid,
    ) -> Result<vgames_proto::versions::CompatProfileList, crate::compat::profiles::ProfileError>
    {
        Ok(vgames_proto::versions::CompatProfileList { items: Vec::new() })
    }
    async fn trust(&self, _: uuid::Uuid) -> Option<Arc<vgames_core::trust::TrustState>> {
        None
    }
}

/// The overlay's part of a launch: the broker's endpoint in the game's environment.
struct OverlayEnv(Vec<(String, String)>);

impl crate::launch::orchestrate::LaunchHooks for OverlayEnv {
    fn before_spawn<'a>(
        &'a self,
        _: &'a crate::launch::hooks::LaunchContext,
    ) -> crate::launch::hooks::HookFuture<
        'a,
        Result<crate::launch::hooks::HookEnv, crate::launch::hooks::LaunchVeto>,
    > {
        let env = self.0.clone();
        Box::pin(async move { Ok(crate::launch::hooks::HookEnv(env)) })
    }

    fn aborted(&self, _: &crate::launch::hooks::LaunchContext) {}
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "downloads real Proton; run by compat-e2e.yml on ubuntu-24.04"]
async fn the_d3d11_test_game_runs_through_proton() {
    let entries: Vec<serde_json::Value> = serde_json::from_slice(
        &std::fs::read(std::env::var("VGAMES_PROTON_E2E_RUNTIMES").unwrap()).unwrap(),
    )
    .unwrap();
    let game = std::fs::read(std::env::var("VGAMES_PROTON_E2E_GAME").unwrap()).unwrap();
    if let Ok(dir) = std::env::var("VGAMES_PROTON_E2E_LOG_DIR") {
        std::fs::create_dir_all(dir).unwrap();
    }
    let expect = std::env::var("VGAMES_PROTON_E2E_EXPECT").unwrap_or_else(|_| "ok".into());

    // The install: a windows-x86_64 package whose target is the test game.
    let game: &'static [u8] = Box::leak(game.into_boxed_slice());
    let files = [FileSpec {
        path: "Game/game.exe".into(),
        size: game.len() as u64,
        content: Content::Literal(game),
        executable: true,
    }];
    let execution = Execution {
        launch: Some(Launch {
            default: "play".into(),
            targets: vec![LaunchTarget {
                id: "play".into(),
                label: "Play".into(),
                executable: "Game/game.exe".into(),
                args: vec!["--frames".into(), "120".into()],
                working_dir: None,
                // Package env, like any manifest's: `PROTON_LOG*` is not launcher-owned.
                env: std::env::var("VGAMES_PROTON_E2E_LOG_DIR")
                    .map(|dir| {
                        BTreeMap::from([
                            ("PROTON_LOG".to_owned(), "1".to_owned()),
                            ("PROTON_LOG_DIR".to_owned(), dir.clone()),
                            ("DXVK_LOG_LEVEL".to_owned(), "info".to_owned()),
                            ("DXVK_LOG_PATH".to_owned(), dir),
                        ])
                    })
                    .unwrap_or_default(),
            }],
        }),
        ..Execution::default()
    };
    let package = TestPackage::build_executable(
        &files,
        &[],
        Compression::None,
        PACK_SIZE,
        &Identity {
            platform: Platform::WindowsX86_64,
            ..Identity::default()
        },
        &execution,
    );
    let work = tempfile::tempdir().unwrap();
    let library = work.path().join("library");
    let root = library.join("game");
    write_tree(&root, &files, &[]);
    let meta = root.join(META_DIR);
    std::fs::create_dir_all(&meta).unwrap();
    std::fs::write(meta.join(MANIFEST_FILE), &package.manifest).unwrap();
    std::fs::write(meta.join(SIGNATURE_FILE), package.envelope.to_bytes()).unwrap();
    write_record(
        &root,
        &InstallRecord::for_manifest(&package.release().verified, InstallState::Installed),
    )
    .unwrap();
    let data = std::env::var_os("VGAMES_PROTON_E2E_DATA")
        .map_or_else(|| work.path().join("data"), std::path::PathBuf::from);
    std::fs::create_dir_all(&data).unwrap();
    // The launcher database lives in the data directory; the install rows are per run.
    let db = Db::open(&data.join(format!("launcher-{}.db", uuid::Uuid::now_v7()))).unwrap();
    let runtime_db = Db::open(&data.join("runtimes.db")).unwrap();
    let expected = package.expected.clone();
    let library_path = library.to_string_lossy().into_owned();
    db.call(move |conn| {
        conn.execute(
            "INSERT INTO servers (id, url, name, root_public_key, root_fingerprint, added_at)
             VALUES (?1, 'https://example.test', 'Test', zeroblob(32), 'VG1', 0)",
            [expected.server_id.to_string()],
        )?;
        conn.execute(
            "INSERT INTO libraries (id, path, label, created_at) VALUES ('lib', ?1, 'Games', 0)",
            [library_path],
        )?;
        conn.execute(
            "INSERT INTO installs (server_id, package_id, library_id, dir_name, version_id,
             sequence, platform, state)
             VALUES (?1, ?2, 'lib', 'game', ?3, ?4, ?5, 'installed')",
            rusqlite::params![
                expected.server_id.to_string(),
                expected.package_id.to_string(),
                expected.version_id.to_string(),
                i64::try_from(expected.sequence).unwrap(),
                expected.platform.as_str(),
            ],
        )?;
        Ok(())
    })
    .await
    .unwrap();

    // The runtime catalog: real upstream archives, signed by a key made for this run.
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let public = key.pk.to_box().unwrap().to_string();
    let catalog = serde_json::to_vec(&serde_json::json!({
        "format": "vgames.runtimes/1", "version": 1,
        "generated_at": "2026-10-11T00:00:00Z", "commercial": false, "runtimes": entries
    }))
    .unwrap();
    vgames_core::runtimes::parse_and_validate(&catalog).expect("the pinned entries validate");
    let signature = minisign::sign(None, &key.sk, Cursor::new(&catalog), None, None)
        .unwrap()
        .to_string();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/runtimes.json"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(catalog))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/runtimes.json.minisig"))
        .respond_with(ResponseTemplate::new(200).set_body_string(signature))
        .mount(&server)
        .await;
    let url = url::Url::parse(&format!("{}/runtimes.json", server.uri())).unwrap();
    let source = CatalogSource {
        signature_url: url::Url::parse(&format!("{url}.minisig")).unwrap(),
        url,
        key_id: vgames_core::runtimes::key_id(&public).unwrap(),
        public_key: public,
    };

    let runtime_catalog = Arc::new(RuntimeCatalog::new(
        Some(source),
        runtime_db.clone(),
        http_client().unwrap(),
    ));
    let store = Arc::new(RuntimeStore::new(
        data.join("runtimes"),
        runtime_db.clone(),
        RuntimeStore::http_client().unwrap(),
    ));
    let planner = CompatPlanner::new(
        data.clone(),
        db.clone(),
        runtime_catalog,
        Arc::clone(&store),
        Arc::new(Profiles::new(Arc::new(NoProfiles), db.clone())),
        Arc::new(crate::compat::preflight::System),
        Some(Platform::LinuxX86_64),
    );
    let bus = EventBus::new();
    let mut events = bus.subscribe();
    let launcher = Launcher::new(
        db.clone(),
        Arc::new(Trust(Arc::new(package.trust.clone()))),
        // Like `AppState`: the data directory gives every compat launch its prefix.
        GameSessions::with_runtime(
            db.clone(),
            bus.clone(),
            Some(data.clone()),
            tokio_util::sync::CancellationToken::new(),
        ),
    );
    assert!(launcher.hooks().register_plan_source(Arc::new(planner)));
    // The overlay, as `OverlayService::prepare_launch` sets it up: the layer manifest for
    // this user and a broker whose endpoint the game gets.
    let overlay = std::env::var_os("VGAMES_PROTON_E2E_OVERLAY").map(|library| {
        let dir = crate::overlay::vulkan_layer::manifest_dir(
            std::env::var_os("XDG_DATA_HOME"),
            std::env::var_os("HOME"),
        )
        .expect("a data directory for the layer manifest");
        let registered =
            crate::overlay::vulkan_layer::sync(&dir, Some(std::path::Path::new(&library))).unwrap();
        println!("overlay layer: {registered:?} in {}", dir.display());
        dir
    });
    let (_views, views) =
        tokio::sync::watch::channel(crate::overlay::model::OverlayView::default());
    let (_panel, panel) = tokio::sync::watch::channel(false);
    let (actions, _actions) = tokio::sync::mpsc::channel(8);
    let (broker_events, mut broker_seen) = tokio::sync::mpsc::channel(16);
    let stop = tokio_util::sync::CancellationToken::new();
    let _broker = match overlay {
        Some(_) => {
            let broker =
                crate::overlay::broker::Broker::start(views, panel, actions, broker_events, &stop)
                    .await
                    .unwrap();
            assert!(launcher.hooks().register(
                crate::launch::hooks::HookStage::Overlay,
                Arc::new(OverlayEnv(broker.endpoint.env())),
            ));
            Some(broker)
        }
        None => None,
    };
    let package_ref = PackageRef {
        server_id: package.expected.server_id,
        package_id: package.expected.package_id,
    };
    let started = launcher.launch(package_ref, TargetChoice::Default).await;

    if expect == "refused-before-extraction" {
        let err = started.expect_err("a tampered runtime must not launch");
        assert!(err.to_string().contains("SHA-256"), "{err}");
        let runtimes = data.join("runtimes");
        let installed: Vec<_> = std::fs::read_dir(&runtimes)
            .map(|d| d.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default();
        println!("runtimes after the refusal: {installed:?}");
        assert!(
            store
                .list()
                .await
                .unwrap()
                .iter()
                .all(|r| r.runtime_id != "umu-proton"),
            "the tampered Proton was recorded"
        );
        assert!(
            !runtimes.join("umu-proton").exists(),
            "the tampered Proton was extracted"
        );
        return;
    }

    let pid = started.unwrap_or_else(|e| panic!("launch refused: {e}"));
    println!("game started through Proton, pid {pid}");
    let stopped = loop {
        let event = tokio::time::timeout(Duration::from_secs(900), events.recv())
            .await
            .expect("the game did not stop within 15 minutes");
        if let Ok(AppEvent::GameStopped(stopped)) = event {
            break stopped;
        }
        if let Err(broadcast::error::RecvError::Closed) = event {
            panic!("event bus closed");
        }
    };
    println!("game stopped: {:?}", stopped.exit);
    assert_eq!(
        stopped.exit.code,
        Some(0),
        "the test game reports the failing step as its exit code"
    );
    let prefix = crate::compat::prefix_dir(&data, &package_ref);
    assert!(
        prefix.join("drive_c").is_dir(),
        "Proton created the prefix where prefix_dir says"
    );
    if let Some(dir) = overlay {
        let mut seen = Vec::new();
        while let Ok(event) = broker_seen.try_recv() {
            seen.push(event);
        }
        println!("overlay broker: {seen:?}");
        crate::overlay::vulkan_layer::sync(&dir, None).unwrap();
        assert!(
            seen.contains(&crate::overlay::broker::BrokerEvent::Connected(
                vgames_overlay::protocol::RendererKind::Vulkan
            )),
            "the Vulkan layer did not connect from inside the Proton game: {seen:?}"
        );
    }
}
