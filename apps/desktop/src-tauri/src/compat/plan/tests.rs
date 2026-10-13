use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Mutex;

use uuid::Uuid;
use vgames_core::compat::is_launcher_owned_env_key;
use vgames_proto::versions::{CompatProfileList, CompatStatus, CompatTarget, SignedCompatProfile};
use vgames_transfer::testkit::package::{PACKAGE_ID, SERVER_ID, publisher_key, trust_state};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::compat::catalog::{CatalogSource, http_client};
use crate::compat::preflight::PreflightError;
use crate::compat::profiles::ProfileError;
use crate::db::Db;

const PACKAGE: PackageRef = PackageRef {
    server_id: SERVER_ID,
    package_id: PACKAGE_ID,
};

#[test]
fn versions_compare_naturally() {
    let mut v = [
        "GE-Proton10-17",
        "GE-Proton9-27",
        "GE-Proton10-2",
        "GE-Proton10-2a",
        "1.10",
        "1.9",
    ];
    v.sort_by(|a, b| natural_cmp(a, b));
    assert_eq!(
        v,
        [
            "1.9",
            "1.10",
            "GE-Proton9-27",
            "GE-Proton10-2",
            "GE-Proton10-2a",
            "GE-Proton10-17"
        ]
    );
    assert_eq!(
        natural_cmp("UMU-Proton-9.0-3", "UMU-Proton-9.0-3"),
        Ordering::Equal
    );
    assert_eq!(natural_cmp("a007", "a7"), Ordering::Equal);
}

fn entry(id: &str, version: &str, min_launcher: &str) -> serde_json::Value {
    let os = if matches!(id, "wine-macos" | "dxmt" | "dxvk-macos" | "moltenvk") {
        "macos"
    } else {
        "linux"
    };
    serde_json::json!({
        "id": id, "version": version, "os": os, "arch": "x86_64",
        "url": format!("https://github.com/example/{id}/releases/download/{version}/{id}.tar.gz"),
        "sha256": format!("{:064x}", version.len() * 7 + id.len()),
        "size": 100, "archive": "tar.gz", "license": "MIT",
        "min_launcher_version": min_launcher
    })
}

fn catalog_bytes(entries: Vec<serde_json::Value>) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "format": "vgames.runtimes/1", "version": 1,
        "generated_at": "2026-10-01T00:00:00Z", "commercial": false, "runtimes": entries
    }))
    .unwrap()
}

#[test]
fn newest_respects_launcher_and_profile_minimums() {
    let c: Catalog = serde_json::from_slice(&catalog_bytes(vec![
        entry("ge-proton", "GE-Proton10-2", "0.1.0"),
        entry("ge-proton", "GE-Proton10-17", "0.1.0"),
        entry("ge-proton", "GE-Proton11-1", "9.0.0"),
        entry("umu-proton", "UMU-Proton-9.0-3", "0.1.0"),
    ]))
    .unwrap();
    let pick = |min| {
        newest(
            &c,
            RuntimeId::GeProton,
            Os::Linux,
            Arch::X86_64,
            "0.1.0",
            min,
        )
        .map(|r| r.version.as_str())
    };
    assert_eq!(
        pick(None),
        Some("GE-Proton10-17"),
        "11-1 needs a newer launcher"
    );
    assert_eq!(pick(Some("GE-Proton10-17")), Some("GE-Proton10-17"));
    assert_eq!(pick(Some("GE-Proton10-18")), None);
    assert_eq!(
        newest(
            &c,
            RuntimeId::GeProton,
            Os::Linux,
            Arch::Aarch64,
            "0.1.0",
            None
        ),
        None
    );
}

struct FakeProfiles(Mutex<Vec<SignedCompatProfile>>);

impl ProfileSource for FakeProfiles {
    async fn fetch(&self, _: Uuid, _: Uuid) -> Result<CompatProfileList, ProfileError> {
        Ok(CompatProfileList {
            items: self.0.lock().unwrap().clone(),
        })
    }
    async fn trust(&self, _: Uuid) -> Option<Arc<vgames_core::trust::TrustState>> {
        Some(Arc::new(trust_state()))
    }
}

struct FakeProbes(Mutex<Option<PreflightError>>, std::sync::atomic::AtomicBool);

impl Probes for FakeProbes {
    fn vulkan_devices(&self) -> Option<usize> {
        match *self.0.lock().unwrap() {
            Some(PreflightError::NoVulkan) => Some(0),
            _ => Some(1),
        }
    }
    fn sysctl(&self, _: &str) -> Option<String> {
        match *self.0.lock().unwrap() {
            Some(PreflightError::UserNamespacesRestricted { .. }) => Some("1".into()),
            _ => None,
        }
    }
    fn free_space(&self, _: &Path) -> Option<u64> {
        Some(100 * preflight::PREFIX_SPACE_BYTES)
    }
    fn x86_64_runs(&self) -> bool {
        self.1.load(std::sync::atomic::Ordering::SeqCst)
    }
}

fn signed_profile(runner: serde_json::Value) -> SignedCompatProfile {
    signed_profile_for("linux", runner)
}

fn signed_profile_for(target: &str, runner: serde_json::Value) -> SignedCompatProfile {
    use base64::Engine as _;
    let bytes = serde_json::to_vec(&serde_json::json!({
        "format": "vgames.compat/1",
        "server_id": SERVER_ID, "package_id": PACKAGE_ID,
        "target": target, "revision": 1,
        "created_at": "2026-10-01T10:00:00Z",
        "applies_to": {"platform": "windows-x86_64", "min_sequence": 1},
        "status": "verified",
        "runner": runner
    }))
    .unwrap();
    let envelope =
        vgames_core::Envelope::sign(&publisher_key(), vgames_core::Context::Compat, &bytes);
    SignedCompatProfile {
        target: if target == "linux" {
            CompatTarget::Linux
        } else {
            CompatTarget::Macos
        },
        revision: 1,
        status: CompatStatus::Verified,
        document: base64::engine::general_purpose::STANDARD.encode(&bytes),
        signature: serde_json::from_slice(&serde_json::to_vec(&envelope).unwrap()).unwrap(),
        created_at: time::OffsetDateTime::UNIX_EPOCH,
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    data: PathBuf,
    _server: MockServer,
    profiles: Arc<FakeProfiles>,
    probes: Arc<FakeProbes>,
    planner: CompatPlanner<FakeProfiles, FakeProbes>,
}

/// A planner whose catalog lists umu-launcher and UMU-Proton, both already
/// installed (the store never downloads here), with an `umu-run` that logs its
/// arguments and prefix.
async fn rig() -> Rig {
    rig_for(Platform::LinuxX86_64).await
}

/// The fake `wine`: `wineboot` creates the prefix; every call is logged.
const FAKE_WINE: &str = "#!/bin/sh\necho \"$*\" >> \"$WINEPREFIX/../wine.log\"\n[ \"$1\" = wineboot ] && mkdir -p \"$WINEPREFIX/drive_c/windows/system32\"\nexit 0\n";

async fn rig_for(host: Platform) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let server = MockServer::start().await;
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let public = key.pk.to_box().unwrap().to_string();
    let entries = vec![
        entry("umu-launcher", "1.2.9", "0.1.0"),
        entry("umu-proton", "UMU-Proton-9.0-3", "0.1.0"),
        entry("umu-proton", "UMU-Proton-9.0-4e", "0.1.0"),
        entry("ge-proton", "GE-Proton10-17", "0.1.0"),
        entry("wine-macos", "10.0", "0.1.0"),
        entry("dxmt", "0.60", "0.1.0"),
        entry("dxvk-macos", "1.10.3", "0.1.0"),
    ];
    let bytes = catalog_bytes(entries);
    let sig = minisign::sign(None, &key.sk, Cursor::new(&bytes), None, None)
        .unwrap()
        .to_string();
    Mock::given(method("GET"))
        .and(path("/runtimes.json"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.clone()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/runtimes.json.minisig"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sig))
        .mount(&server)
        .await;
    let url = url::Url::parse(&format!("{}/runtimes.json", server.uri())).unwrap();
    let source = CatalogSource {
        signature_url: url::Url::parse(&format!("{url}.minisig")).unwrap(),
        url,
        key_id: vgames_core::runtimes::key_id(&public).unwrap(),
        public_key: public,
    };
    let db = Db::open_in_memory().unwrap();
    let catalog = Arc::new(RuntimeCatalog::new(
        Some(source),
        db.clone(),
        http_client().unwrap(),
    ));
    let store = Arc::new(RuntimeStore::new(
        data.join("runtimes"),
        db.clone(),
        RuntimeStore::http_client().unwrap(),
    ));
    let parsed: Catalog = serde_json::from_slice(&bytes).unwrap();
    for rt in &parsed.runtimes {
        let dir = store.dir(rt.id, &rt.version);
        match rt.id {
            RuntimeId::UmuLauncher => {
                std::fs::create_dir_all(dir.join("umu")).unwrap();
                let script = dir.join("umu/umu-run");
                std::fs::write(
                    &script,
                    "#!/bin/sh\necho \"$WINEPREFIX|$PROTONPATH|$GAMEID|$*\" >> \"$WINEPREFIX/../umu.log\"\n",
                )
                .unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                        .unwrap();
                }
            }
            RuntimeId::WineMacos => {
                let bin = dir.join("Wine.app/Contents/Resources/wine/bin");
                std::fs::create_dir_all(&bin).unwrap();
                std::fs::create_dir_all(
                    dir.join("Wine.app/Contents/Resources/wine/lib/wine/x86_64-unix"),
                )
                .unwrap();
                std::fs::write(bin.join("wine"), FAKE_WINE).unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    std::fs::set_permissions(
                        bin.join("wine"),
                        std::fs::Permissions::from_mode(0o755),
                    )
                    .unwrap();
                }
            }
            RuntimeId::Dxmt => {
                std::fs::create_dir_all(dir.join("dxmt/x86_64-windows")).unwrap();
                std::fs::create_dir_all(dir.join("dxmt/x86_64-unix")).unwrap();
                std::fs::write(dir.join("dxmt/x86_64-windows/d3d11.dll"), b"dxmt").unwrap();
                std::fs::write(dir.join("dxmt/x86_64-unix/winemetal.so"), b"bridge").unwrap();
            }
            RuntimeId::DxvkMacos => {
                std::fs::create_dir_all(dir.join("dxvk/x64")).unwrap();
                std::fs::write(dir.join("dxvk/x64/d3d9.dll"), b"dxvk").unwrap();
            }
            _ => {
                std::fs::create_dir_all(dir.join(&rt.version)).unwrap();
                std::fs::write(
                    dir.join(&rt.version).join("proton"),
                    "#!/usr/bin/env python3\n",
                )
                .unwrap();
            }
        }
        crate::compat::runtimes::record(&db, rt).await.unwrap();
    }
    let profiles = Arc::new(FakeProfiles(Mutex::new(Vec::new())));
    let probes = Arc::new(FakeProbes(
        Mutex::new(None),
        std::sync::atomic::AtomicBool::new(true),
    ));
    let planner = CompatPlanner::new(
        data.clone(),
        db.clone(),
        catalog,
        store,
        Arc::new(Profiles::new(Arc::clone(&profiles), db.clone())),
        Arc::clone(&probes),
        Some(host),
    );
    Rig {
        _dir: dir,
        data,
        _server: server,
        profiles,
        probes,
        planner,
    }
}

fn release(platform: Platform) -> ExpectedRelease {
    ExpectedRelease {
        server_id: SERVER_ID,
        package_id: PACKAGE_ID,
        version_id: Uuid::from_u128(9),
        platform,
        sequence: 1,
    }
}

#[tokio::test]
async fn a_windows_build_on_linux_gets_a_proton_plan_from_verified_parts() {
    let r = rig().await;
    *r.profiles.0.lock().unwrap() = vec![signed_profile(serde_json::json!({
        "kind": "proton",
        "prefer": ["ge-proton"],
        "umu_game_id": "umu-12345",
        "env": {"DXVK_HUD": "fps"},
        "dll_overrides": {"d3dcompiler_47": "native,builtin"}
    }))];
    let LaunchPlan::Proton(plan) = r
        .planner
        .plan_for(
            PACKAGE,
            &release(Platform::WindowsX86_64),
            Path::new("/games/x/Game.exe"),
        )
        .await
        .unwrap()
    else {
        panic!("not a Proton plan");
    };
    let runtimes = r.data.join("runtimes");
    assert_eq!(
        plan.umu_run,
        runtimes.join("umu-launcher/1.2.9/umu/umu-run")
    );
    assert_eq!(
        plan.proton_path,
        runtimes.join("ge-proton/GE-Proton10-17/GE-Proton10-17"),
        "the profile's preference"
    );
    assert_eq!(plan.prefix, crate::compat::prefix_dir(&r.data, &PACKAGE));
    assert!(plan.prefix.is_dir());
    assert_eq!(plan.umu_game_id.as_deref(), Some("umu-12345"));
    assert_eq!(plan.store, "none");
    assert_eq!(plan.env.get("DXVK_HUD").map(String::as_str), Some("fps"));
    assert_eq!(plan.dll_overrides, "d3dcompiler_47=native,builtin");
    assert!(plan.env.keys().all(|k| !is_launcher_owned_env_key(k)));

    // The plan's process: umu-run with the game first, launcher-owned variables set.
    let target = crate::launch::ResolvedTarget {
        executable: PathBuf::from("/games/x/Game.exe"),
        args: vec!["-windowed".into()],
        working_dir: PathBuf::from("/games/x"),
        env: BTreeMap::new(),
        id: "play".into(),
    };
    let prepared = LaunchPlan::Proton(plan.clone())
        .prepare(target, Vec::new())
        .unwrap();
    assert_eq!(prepared.program, plan.umu_run);
    assert_eq!(prepared.args, ["/games/x/Game.exe", "-windowed"]);
    assert_eq!(
        prepared.env.get("WINEPREFIX").unwrap(),
        plan.prefix.as_os_str()
    );
    assert_eq!(
        prepared.env.get("PROTONPATH").unwrap(),
        plan.proton_path.as_os_str()
    );
    assert_eq!(prepared.env.get("GAMEID").unwrap(), "umu-12345");
}

#[tokio::test]
async fn without_a_profile_the_newest_umu_proton_is_used() {
    let r = rig().await;
    let LaunchPlan::Proton(plan) = r
        .planner
        .plan_for(
            PACKAGE,
            &release(Platform::WindowsX86_64),
            Path::new("/games/x/Game.exe"),
        )
        .await
        .unwrap()
    else {
        panic!("not a Proton plan");
    };
    assert!(
        plan.proton_path
            .ends_with("umu-proton/UMU-Proton-9.0-4e/UMU-Proton-9.0-4e"),
        "{}",
        plan.proton_path.display()
    );
    assert!(plan.env.is_empty() && plan.dll_overrides.is_empty() && plan.umu_game_id.is_none());
}

#[tokio::test]
async fn a_failed_preflight_refuses_with_its_message() {
    let r = rig().await;
    *r.probes.0.lock().unwrap() = Some(PreflightError::NoVulkan);
    let err = r
        .planner
        .plan_for(
            PACKAGE,
            &release(Platform::WindowsX86_64),
            Path::new("/games/x/Game.exe"),
        )
        .await
        .unwrap_err();
    assert_eq!(
        err,
        LaunchError::CompatUnavailable {
            detail: PreflightError::NoVulkan.to_string()
        }
    );
    *r.probes.0.lock().unwrap() = Some(PreflightError::UserNamespacesRestricted { sysctl: "" });
    let err = r
        .planner
        .plan_for(
            PACKAGE,
            &release(Platform::WindowsX86_64),
            Path::new("/games/x/Game.exe"),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains(preflight::APPARMOR_USERNS_SYSCTL),
        "{err}"
    );
}

#[tokio::test]
async fn other_hosts_and_builds_are_refused() {
    let r = rig().await;
    for platform in [Platform::WindowsAarch64, Platform::MacosX86_64] {
        assert!(matches!(
            r.planner
                .plan_for(PACKAGE, &release(platform), Path::new("/games/x/Game.exe"))
                .await,
            Err(LaunchError::CompatUnavailable { .. })
        ));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn winetricks_verbs_run_once_per_prefix() {
    let r = rig().await;
    *r.profiles.0.lock().unwrap() = vec![signed_profile(serde_json::json!({
        "kind": "proton", "winetricks": ["vcrun2022", "d3dcompiler_47"]
    }))];
    for _ in 0..2 {
        r.planner
            .plan_for(
                PACKAGE,
                &release(Platform::WindowsX86_64),
                Path::new("/games/x/Game.exe"),
            )
            .await
            .unwrap();
    }
    let prefix = crate::compat::prefix_dir(&r.data, &PACKAGE);
    let log = std::fs::read_to_string(prefix.parent().unwrap().join("umu.log")).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "{log}");
    assert!(lines[0].starts_with(&format!("{}|", prefix.display())));
    assert!(
        lines[0].ends_with("|0|winetricks vcrun2022"),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].ends_with("|winetricks d3dcompiler_47"),
        "{}",
        lines[1]
    );
}

fn windows_manifest() -> vgames_core::manifest::Manifest {
    let package = vgames_transfer::testkit::TestPackage::build_executable(
        &[vgames_transfer::testkit::FileSpec::random(
            "Game/Game.exe",
            64,
            1,
        )],
        &[],
        vgames_pack::Compression::None,
        vgames_pack::PACK_SIZE,
        &vgames_transfer::testkit::Identity {
            platform: Platform::WindowsX86_64,
            ..vgames_transfer::testkit::Identity::default()
        },
        &vgames_pack::manifest::Execution::default(),
    );
    package.release().manifest().clone()
}

fn context(r: &Rig) -> crate::launch::hooks::LaunchContext {
    crate::launch::hooks::LaunchContext {
        package: PACKAGE,
        root: r.data.join("library/game"),
        platform: Platform::WindowsX86_64,
        layer: crate::launch::hooks::LaunchLayer::Proton,
        prefix_dir: Some(crate::compat::prefix_dir(&r.data, &PACKAGE)),
        cancellation: tokio_util::sync::CancellationToken::new(),
    }
}

#[tokio::test]
async fn the_launcher_hook_plans_from_the_verified_manifest() {
    use crate::launch::hooks::PlanSource as _;
    let r = rig().await;
    let manifest = windows_manifest();
    let ctx = context(&r);
    let plan = r.planner.plan(&ctx, &manifest).await.unwrap();
    let LaunchPlan::Proton(plan) = plan else {
        panic!("not a Proton plan");
    };
    assert_eq!(Some(plan.prefix), ctx.prefix_dir);

    let cancelled = context(&r);
    cancelled.cancellation.cancel();
    assert!(matches!(
        r.planner.plan(&cancelled, &manifest).await,
        Err(LaunchError::CompatUnavailable { .. })
    ));
}

// The fake `wine` is a shell script: Unix hosts only (Wine runs on macOS; Windows needs none).
#[cfg(unix)]
mod wine {
    use super::*;
    use crate::compat::pe::tests::pe;
    use crate::compat::plan::wine::{D3D12_ON_MAC, NEEDS_ROSETTA, graphics_chain};
    use vgames_core::compat::Graphics;

    fn game(dir: &Path, imports: &[&str]) -> PathBuf {
        let path = dir.join("Game.exe");
        std::fs::write(&path, pe(imports, &[], false)).unwrap();
        path
    }

    async fn wine_plan(r: &Rig, exe: &Path) -> Result<crate::launch::WinePlan, LaunchError> {
        match r
            .planner
            .plan_for(PACKAGE, &release(Platform::WindowsX86_64), exe)
            .await?
        {
            LaunchPlan::Wine(plan) => Ok(plan),
            other => panic!("not a Wine plan: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_d3d11_game_gets_dxmt_with_its_dlls_and_overrides() {
        let r = rig_for(Platform::MacosAarch64).await;
        let exe = game(&r.data, &["d3d11.dll"]);
        let plan = wine_plan(&r, &exe).await.unwrap();
        assert_eq!(plan.graphics, Graphics::Dxmt);
        assert!(
            plan.wine
                .ends_with("Wine.app/Contents/Resources/wine/bin/wine")
        );
        assert_eq!(plan.prefix, crate::compat::prefix_dir(&r.data, &PACKAGE));
        assert_eq!(plan.dll_overrides, "d3d11,d3d10core,dxgi=n,b");
        assert_eq!(
            std::fs::read(plan.prefix.join("drive_c/windows/system32/d3d11.dll")).unwrap(),
            b"dxmt"
        );
        let log = std::fs::read_to_string(plan.prefix.parent().unwrap().join("wine.log")).unwrap();
        assert!(
            log.starts_with(
                "wineboot --init\nreg add HKCU\\Software\\Wine\\Mac Driver /v RetinaMode"
            ),
            "{log}"
        );
        // The prefix exists now: a second plan does not boot it again.
        wine_plan(&r, &exe).await.unwrap();
        let log = std::fs::read_to_string(plan.prefix.parent().unwrap().join("wine.log")).unwrap();
        assert_eq!(log.lines().count(), 2, "{log}");
    }

    #[tokio::test]
    async fn d3d9_only_skips_dxmt_and_d3d12_is_refused_before_anything_runs() {
        let r = rig_for(Platform::MacosX86_64).await;
        let plan = wine_plan(&r, &game(&r.data, &["d3d9.dll"])).await.unwrap();
        assert_eq!(plan.graphics, Graphics::Dxvk);
        assert_eq!(plan.dll_overrides, "d3d9,d3d11,d3d10core,dxgi=n,b");

        let r = rig_for(Platform::MacosAarch64).await;
        let err = wine_plan(&r, &game(&r.data, &["kernel32.dll", "d3d12.dll"]))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            LaunchError::CompatUnavailable {
                detail: D3D12_ON_MAC.into()
            }
        );
        assert!(
            !crate::compat::prefix_dir(&r.data, &PACKAGE).exists(),
            "nothing was prepared"
        );
    }

    #[tokio::test]
    async fn without_rosetta_the_launch_asks_for_it() {
        let r = rig_for(Platform::MacosAarch64).await;
        r.probes.1.store(false, std::sync::atomic::Ordering::SeqCst);
        let err = wine_plan(&r, &game(&r.data, &["d3d11.dll"]))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            LaunchError::CompatUnavailable {
                detail: NEEDS_ROSETTA.into()
            }
        );
    }

    #[tokio::test]
    async fn a_listed_d3dmetal_is_skipped_for_the_next_backend() {
        let r = rig_for(Platform::MacosAarch64).await;
        *r.profiles.0.lock().unwrap() = vec![signed_profile_for(
            "macos",
            serde_json::json!({
                "kind": "wine",
                "graphics": ["d3dmetal", "wined3d"],
                "dll_overrides": {"xinput1_3": "native"}
            }),
        )];
        let plan = wine_plan(&r, &game(&r.data, &["d3d11.dll"])).await.unwrap();
        assert_eq!(plan.graphics, Graphics::Wined3d);
        assert_eq!(plan.dll_overrides, "xinput1_3=native");
    }

    #[test]
    fn the_default_chain_has_no_d3dmetal() {
        assert_eq!(
            graphics_chain(None),
            [Graphics::Dxmt, Graphics::Dxvk, Graphics::Wined3d]
        );
    }
}
