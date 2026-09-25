//! A hard process stop after a confirmed piece, followed by a new process's
//! persisted resume record against a GCS-style simulator.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use time::OffsetDateTime;
use uuid::Uuid;
use vgames_pack::scan::{FsReader, scan};
use vgames_pack::{PACK_SIZE, PackSource, Plan};
use vgames_proto::versions::{UploadMethod, UploadTarget};
use vgames_transfer::download::RemoteError;
use vgames_transfer::upload::{PIECE_SIZE, UploadApi, UploadControl, UploadOptions, run};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

struct Api(String);

impl UploadApi for Api {
    async fn pack_upload_target(
        &self,
        _version_id: Uuid,
        _pack: u32,
    ) -> Result<UploadTarget, RemoteError> {
        Ok(UploadTarget {
            url: format!("{}/start", self.0),
            method: UploadMethod::Post,
            headers: [("x-goog-resumable".to_owned(), "start".to_owned())].into(),
            expires_at: OffsetDateTime::now_utc() + time::Duration::minutes(15),
        })
    }
}

fn source(root: &Path) -> PackSource {
    let scan = scan(root).unwrap();
    let plan = Arc::new(Plan::new(scan.files, scan.directories).unwrap());
    let packing = Arc::new(plan.raw_packing(PACK_SIZE).unwrap());
    PackSource::new(plan, packing, Arc::new(FsReader::new(root)))
}

fn options() -> UploadOptions {
    let mut options = UploadOptions::default();
    options.client_options.use_system_proxy = false;
    options
}

#[tokio::test]
#[ignore = "spawned as the upload process by hard_stop_then_resume"]
async fn child_entry() {
    let root = std::env::var_os("VGAMES_UPLOAD_TEST_ROOT").unwrap();
    let url = std::env::var("VGAMES_UPLOAD_TEST_URL").unwrap();
    let version = std::env::var("VGAMES_UPLOAD_TEST_VERSION")
        .unwrap()
        .parse()
        .unwrap();
    let root = Path::new(&root);
    run(
        source(&root.join("source")),
        version,
        Arc::new(Api(url)),
        root.join("resume.json"),
        &options(),
        &UploadControl::new(),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn hard_stop_then_resume() {
    let dir = tempfile::tempdir().unwrap();
    let files = dir.path().join("source");
    std::fs::create_dir(&files).unwrap();
    let content = vec![b'c'; PIECE_SIZE + 777];
    std::fs::write(files.join("game"), &content).unwrap();
    let mut random = [0u8; 1];
    getrandom::fill(&mut random).unwrap();
    let accepted = (u64::from(random[0] % 63) + 1) * 256 * 1024;
    let server = MockServer::start().await;
    let session = format!("{}/session", server.uri());
    Mock::given(method("POST"))
        .and(path("/start"))
        .respond_with(ResponseTemplate::new(201).insert_header("location", session.as_str()))
        .expect(1)
        .mount(&server)
        .await;
    let finish = Arc::new(AtomicBool::new(false));
    let allow = finish.clone();
    Mock::given(method("PUT"))
        .and(path("/session"))
        .respond_with(move |request: &Request| {
            let range = request
                .headers
                .get("content-range")
                .unwrap()
                .to_str()
                .unwrap();
            if range == "bytes */*" {
                return ResponseTemplate::new(308)
                    .insert_header("range", format!("bytes=0-{}", accepted - 1));
            }
            if range.starts_with("bytes 0-") {
                assert_eq!(request.body.len(), PIECE_SIZE);
                return ResponseTemplate::new(308)
                    .insert_header("range", format!("bytes=0-{}", accepted - 1));
            }
            assert_eq!(
                range,
                format!(
                    "bytes {}-{}/{}",
                    accepted,
                    PIECE_SIZE + 776,
                    PIECE_SIZE + 777
                )
            );
            assert_eq!(request.body.len(), PIECE_SIZE + 777 - accepted as usize);
            assert!(request.body.iter().all(|&byte| byte == b'c'));
            if allow.load(Ordering::SeqCst) {
                ResponseTemplate::new(200)
            } else {
                ResponseTemplate::new(503).set_delay(Duration::from_secs(5))
            }
        })
        .mount(&server)
        .await;
    let version = Uuid::now_v7();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "child_entry"])
        .env("VGAMES_UPLOAD_TEST_ROOT", dir.path())
        .env("VGAMES_UPLOAD_TEST_URL", server.uri())
        .env("VGAMES_UPLOAD_TEST_VERSION", version.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let record = dir.path().join("resume.json");
    let ready = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(bytes) = std::fs::read(&record)
                && let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && json["packs"][0]["offset"].as_u64() == Some(accepted)
            {
                break;
            }
            if child.try_wait().unwrap().is_some() {
                panic!("upload child exited before checkpoint");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if ready.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    ready.unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    finish.store(true, Ordering::SeqCst);
    run(
        source(&files),
        version,
        Arc::new(Api(server.uri())),
        record,
        &options(),
        &UploadControl::new(),
    )
    .await
    .unwrap();
    let requests = server.received_requests().await.unwrap();
    assert!(requests.iter().any(|request| {
        request
            .headers
            .get("content-range")
            .is_some_and(|value| value == "bytes */*")
    }));
}
