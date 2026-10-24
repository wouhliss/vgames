//! The D3D11 test game through real Wine on macOS (GAME-06). Ignored by default; the
//! hosted `compat-e2e.yml` job `wine` runs it on `macos-15` with:
//!
//! - `VGAMES_WINE_E2E_RUNTIMES`: runtime-catalog entries pinning the real upstream
//!   Wine and graphics backends (`scripts/ci/wine-runtimes.sh`);
//! - `VGAMES_WINE_E2E_GAME`: the Windows test game;
//! - `VGAMES_WINE_E2E_GRAPHICS`: the backend the profile asks for (`dxmt`, `dxvk`, `wined3d`).
//!
//! macOS game tracking is PLAY-03's; until it lands this spawns
//! `PreparedLaunch::command()` directly, as GAME-06 specifies.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::compat::catalog::{CatalogSource, http_client};
use crate::db::Db;
use crate::launch::ResolvedTarget;

struct AskFor(String);

async fn record_bootstrap_state(prefix: &std::path::Path) {
    let mut probe = tokio::process::Command::new("/bin/ps");
    probe.args(["-axo", "command="]).kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(5), probe.output()).await;
    let counts = match output {
        Ok(Ok(output)) if output.status.success() && output.stdout.len() <= 1024 * 1024 => {
            Some(super::bootstrap_diagnostics::bootstrap_process_counts(
                &String::from_utf8_lossy(&output.stdout),
            ))
        }
        _ => None,
    };
    // Never emit command lines, environment values, registry contents or probe stderr.
    println!(
        "Wine bootstrap state: {}",
        serde_json::json!({
            "process_counts": counts,
            "drive_c_exists": prefix.join("drive_c").is_dir(),
            "system_registry_exists": prefix.join("system.reg").is_file(),
            "user_registry_exists": prefix.join("user.reg").is_file(),
        })
    );
}

impl ProfileSource for AskFor {
    async fn fetch(
        &self,
        server_id: Uuid,
        package_id: Uuid,
    ) -> Result<vgames_proto::versions::CompatProfileList, crate::compat::profiles::ProfileError>
    {
        use base64::Engine as _;
        let bytes = serde_json::to_vec(&serde_json::json!({
            "format": "vgames.compat/1", "server_id": server_id, "package_id": package_id,
            "target": "macos", "revision": 1, "created_at": "2026-10-11T00:00:00Z",
            "applies_to": {"platform": "windows-x86_64", "min_sequence": 1},
            "status": "untested",
            "runner": {"kind": "wine", "graphics": [self.0]}
        }))
        .unwrap();
        let key = vgames_transfer::testkit::package::publisher_key();
        let envelope = vgames_core::Envelope::sign(&key, vgames_core::Context::Compat, &bytes);
        Ok(vgames_proto::versions::CompatProfileList {
            items: vec![vgames_proto::versions::SignedCompatProfile {
                target: vgames_proto::versions::CompatTarget::Macos,
                revision: 1,
                status: vgames_proto::versions::CompatStatus::Untested,
                document: base64::engine::general_purpose::STANDARD.encode(&bytes),
                signature: serde_json::from_slice(&serde_json::to_vec(&envelope).unwrap()).unwrap(),
                created_at: time::OffsetDateTime::UNIX_EPOCH,
            }],
        })
    }
    async fn trust(&self, _: Uuid) -> Option<Arc<vgames_core::trust::TrustState>> {
        Some(Arc::new(vgames_transfer::testkit::trust_state()))
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "downloads real Wine; run by compat-e2e.yml on macos-15"]
async fn the_d3d11_test_game_runs_through_wine() {
    let entries: Vec<serde_json::Value> = serde_json::from_slice(
        &std::fs::read(std::env::var("VGAMES_WINE_E2E_RUNTIMES").unwrap()).unwrap(),
    )
    .unwrap();
    let game = std::path::PathBuf::from(std::env::var("VGAMES_WINE_E2E_GAME").unwrap());
    let graphics = std::env::var("VGAMES_WINE_E2E_GRAPHICS").unwrap_or_else(|_| "dxmt".into());

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

    let work = tempfile::tempdir().unwrap();
    let data = work.path().join("data");
    let db = Db::open(&work.path().join("launcher.db")).unwrap();
    let planner = CompatPlanner::new(
        data.clone(),
        db.clone(),
        Arc::new(RuntimeCatalog::new(
            Some(source),
            db.clone(),
            http_client().unwrap(),
        )),
        Arc::new(RuntimeStore::new(
            data.join("runtimes"),
            db.clone(),
            RuntimeStore::http_client().unwrap(),
        )),
        Arc::new(Profiles::new(
            Arc::new(AskFor(graphics.clone())),
            db.clone(),
        )),
        Arc::new(crate::compat::preflight::System),
        crate::launch::orchestrate::host_platform(),
    );
    let package = PackageRef {
        server_id: vgames_transfer::testkit::package::SERVER_ID,
        package_id: vgames_transfer::testkit::package::PACKAGE_ID,
    };
    let release = ExpectedRelease {
        server_id: package.server_id,
        package_id: package.package_id,
        version_id: Uuid::now_v7(),
        platform: Platform::WindowsX86_64,
        sequence: 1,
    };
    let prefix = crate::compat::prefix_dir(&data, &package);
    let planned = planner.plan_for(package, &release, &game).await;
    if planned.is_err() {
        record_bootstrap_state(&prefix).await;
    }
    let plan = planned.unwrap_or_else(|e| panic!("plan refused: {e}"));
    let LaunchPlan::Wine(wine) = &plan else {
        panic!("not a Wine plan: {plan:?}");
    };
    println!(
        "Wine plan: {} with {:?}",
        wine.wine.display(),
        wine.graphics
    );
    let target = ResolvedTarget {
        id: "play".into(),
        executable: game.clone(),
        args: vec!["--frames".into(), "120".into()],
        working_dir: game.parent().unwrap().to_owned(),
        env: BTreeMap::new(),
    };
    let prepared = plan.prepare(target, std::env::vars_os()).unwrap();
    let mut command = tokio::process::Command::from(prepared.command());
    command.kill_on_drop(true);
    let status = tokio::time::timeout(Duration::from_secs(600), command.status())
        .await
        .expect("the game did not stop within 10 minutes")
        .unwrap();
    println!("{graphics}: the test game exited with {status}");
    assert_eq!(
        status.code(),
        Some(0),
        "the exit code names the failing step"
    );
}
