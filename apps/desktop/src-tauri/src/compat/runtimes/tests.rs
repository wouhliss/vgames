use std::path::Path;

use uuid::Uuid;
use vgames_core::runtimes::{Arch, ArchiveFormat, Os};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

fn vector(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../crates/vgames-core/tests/vectors/runtimes")
            .join(name),
    )
    .unwrap()
}

fn sha(bytes: &[u8]) -> String {
    hex_lower(&Sha256::digest(bytes))
}

fn runtime(server: &MockServer, id: RuntimeId, version: &str, archive: &[u8]) -> Runtime {
    Runtime {
        id,
        version: version.into(),
        os: Os::Linux,
        arch: Arch::X86_64,
        url: format!("{}/a/{}-{version}.tar.gz", server.uri(), id.as_str()),
        sha256: sha(archive),
        size: archive.len() as u64,
        archive: ArchiveFormat::TarGz,
        license: "MIT".into(),
        min_launcher_version: "0.1.0".into(),
        rosetta_required: false,
        macos_max_supported: None,
        redistribution: None,
    }
}

async fn serve(server: &MockServer, rt: &Runtime, body: Vec<u8>) {
    let p = Url::parse(&rt.url).unwrap().path().to_owned();
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .mount(server)
        .await;
}

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    db: Db,
    store: RuntimeStore,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("runtimes");
    let db = Db::open_in_memory().unwrap();
    let store = RuntimeStore::new(
        root.clone(),
        db.clone(),
        RuntimeStore::http_client().unwrap(),
    );
    Rig {
        _dir: dir,
        root,
        db,
        store,
    }
}

fn package(n: u128) -> PackageRef {
    PackageRef {
        server_id: Uuid::from_u128(1),
        package_id: Uuid::from_u128(n),
    }
}

fn tree(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries {
            let p = e.unwrap().path();
            out.push(p.strip_prefix(root).unwrap().to_string_lossy().into_owned());
            if p.is_dir() {
                stack.push(p);
            }
        }
    }
    out.sort();
    out
}

#[tokio::test]
async fn a_pinned_archive_is_downloaded_verified_and_extracted() {
    let server = MockServer::start().await;
    let r = rig();
    let archive = vector("test-runtime.tar.gz");
    let rt = runtime(&server, RuntimeId::UmuLauncher, "0.0.0-test", &archive);
    serve(&server, &rt, archive).await;
    let dir = r.store.ensure(&rt, package(2)).await.unwrap();
    assert_eq!(dir, r.root.join("umu-launcher/0.0.0-test"));
    assert!(!tree(&dir).is_empty());
    let listed = r.store.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].used_by, 1);
    // Ensured again: nothing is downloaded.
    r.store.ensure(&rt, package(3)).await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert_eq!(r.store.list().await.unwrap()[0].used_by, 2);
    assert!(tree(&r.root.join(".work")).is_empty(), "no leftovers");
}

#[tokio::test]
async fn a_tampered_archive_is_refused_before_anything_is_extracted() {
    let server = MockServer::start().await;
    let r = rig();
    let rt = runtime(
        &server,
        RuntimeId::UmuLauncher,
        "0.0.0-test",
        &vector("test-runtime.tar.gz"),
    );
    serve(&server, &rt, vector("test-runtime-tampered.tar.gz")).await;
    let err = r.store.ensure(&rt, package(2)).await.unwrap_err();
    assert!(matches!(err, RuntimeError::HashMismatch), "{err}");
    assert_eq!(
        tree(&r.root),
        [".work"],
        "no runtime, no staging, no part file"
    );
    assert!(r.store.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn the_pinned_size_is_enforced_while_downloading() {
    let server = MockServer::start().await;
    let r = rig();
    let archive = vector("test-runtime.tar.gz");
    let rt = runtime(&server, RuntimeId::UmuLauncher, "0.0.0-test", &archive);
    let mut longer = archive.clone();
    longer.extend_from_slice(&[0; 4096]);
    serve(&server, &rt, longer).await;
    let err = r.store.ensure(&rt, package(2)).await.unwrap_err();
    assert!(matches!(err, RuntimeError::SizeMismatch), "{err}");
    assert_eq!(tree(&r.root), [".work"]);
}

#[tokio::test]
async fn an_interrupted_download_resumes_with_a_range_request() {
    let server = MockServer::start().await;
    let r = rig();
    let archive = vector("test-runtime.tar.gz");
    let rt = runtime(&server, RuntimeId::UmuLauncher, "0.0.0-test", &archive);
    std::fs::create_dir_all(r.root.join(".work")).unwrap();
    std::fs::write(
        r.root.join(".work/umu-launcher-0.0.0-test.part"),
        &archive[..100],
    )
    .unwrap();
    Mock::given(method("GET"))
        .and(header("range", "bytes=100-"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(archive[100..].to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    r.store.ensure(&rt, package(2)).await.unwrap();
}

#[tokio::test]
async fn redirects_are_followed_within_bounds() {
    let server = MockServer::start().await;
    let r = rig();
    let archive = vector("test-runtime.tar.gz");
    let rt = runtime(&server, RuntimeId::UmuLauncher, "0.0.0-test", &archive);
    Mock::given(method("GET"))
        .and(path("/a/umu-launcher-0.0.0-test.tar.gz"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/assets/blob"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/assets/blob"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(archive))
        .mount(&server)
        .await;
    r.store.ensure(&rt, package(2)).await.unwrap();

    let looping = runtime(&server, RuntimeId::GeProton, "1", &[1]);
    Mock::given(method("GET"))
        .and(path("/a/ge-proton-1.tar.gz"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/a/ge-proton-1.tar.gz"))
        .mount(&server)
        .await;
    assert!(matches!(
        r.store.ensure(&looping, package(2)).await,
        Err(RuntimeError::Network(_))
    ));
}

#[tokio::test]
async fn garbage_collection_keeps_referenced_versions() {
    let server = MockServer::start().await;
    let r = rig();
    let archive = vector("test-runtime.tar.gz");
    let old = runtime(&server, RuntimeId::UmuProton, "1", &archive);
    let new = runtime(&server, RuntimeId::UmuProton, "2", &archive);
    let launcher = runtime(&server, RuntimeId::UmuLauncher, "1", &archive);
    for rt in [&old, &new, &launcher] {
        serve(&server, rt, archive.clone()).await;
    }
    r.store.ensure(&old, package(2)).await.unwrap();
    r.store.ensure(&launcher, package(2)).await.unwrap();
    r.store.ensure(&new, package(3)).await.unwrap();
    // Package 2 now uses version 2 (its reference moves).
    r.store.ensure(&new, package(2)).await.unwrap();

    let installed: HashSet<PackageRef> = [package(2), package(3)].into();
    let removed = r.store.collect_garbage(&installed).await.unwrap();
    assert_eq!(removed, [("umu-proton".to_owned(), "1".to_owned())]);
    assert!(!r.root.join("umu-proton/1").exists());
    assert!(r.root.join("umu-proton/2").exists());

    // Package 2 uninstalled through the hook: its launcher reference goes; 3 keeps v2.
    r.store.package_removed(package(2)).await.unwrap();
    let removed = r.store.collect_garbage(&[package(3)].into()).await.unwrap();
    assert_eq!(removed, [("umu-launcher".to_owned(), "1".to_owned())]);
    assert!(r.root.join("umu-proton/2").exists());

    // Without the hook (INS-04 not wired), counting installs is enough.
    let removed = r.store.collect_garbage(&HashSet::new()).await.unwrap();
    assert_eq!(removed, [("umu-proton".to_owned(), "2".to_owned())]);
    assert!(r.store.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_version_in_use_cannot_be_removed() {
    let server = MockServer::start().await;
    let r = rig();
    let archive = vector("test-runtime.tar.gz");
    let rt = runtime(&server, RuntimeId::UmuProton, "1", &archive);
    serve(&server, &rt, archive).await;
    r.store.ensure(&rt, package(2)).await.unwrap();
    r.store.ensure(&rt, package(3)).await.unwrap();
    assert!(matches!(
        r.store.remove(RuntimeId::UmuProton, "1").await,
        Err(RuntimeError::InUse { used_by: 2 })
    ));
    r.store.package_removed(package(2)).await.unwrap();
    r.store.package_removed(package(3)).await.unwrap();
    r.store.remove(RuntimeId::UmuProton, "1").await.unwrap();
    assert!(!r.root.join("umu-proton/1").exists());
    let _ = &r.db;
}
