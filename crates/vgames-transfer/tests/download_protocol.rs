#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! HTTP framing regressions for A2-T04. Extra bytes must not turn an invalid
//! range into a completed install merely because every chunk was received.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use vgames_pack::Compression;
use vgames_transfer::download::{DownloadControl, DownloadError, DownloadOptions};
use vgames_transfer::http::ClientOptions;
use vgames_transfer::install::{self, InstallError, InstallOutcome, InstallState};
use vgames_transfer::testkit::{FileSpec, Links, MockApi, TestPackage};

struct ChunkedServer {
    url: String,
    requests: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for ChunkedServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ChunkedServer {
    async fn start(body: Arc<Vec<u8>>, always_extra: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicU64::new(0));
        let count = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut reader = BufReader::new(read);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                }
                let request = count.fetch_add(1, Ordering::Relaxed);
                let header = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-{}/{}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",
                    body.len() - 1,
                    body.len(),
                    body.len(),
                );
                if write.write_all(header.as_bytes()).await.is_err()
                    || write.write_all(&body).await.is_err()
                    || write.write_all(b"\r\n").await.is_err()
                {
                    continue;
                }
                // A separate HTTP chunk delivers the byte after the complete
                // signed chunk. No Content-Length lets it evade the header check.
                if (always_extra || request == 0) && write.write_all(b"1\r\nx\r\n").await.is_err() {
                    continue;
                }
                let _ = write.write_all(b"0\r\n\r\n").await;
            }
        });
        Self {
            url,
            requests,
            task,
        }
    }
}

async fn trailing_bytes_case(always_extra: bool) {
    let file = FileSpec::random("data.bin", 1024, 7);
    let package = TestPackage::build(std::slice::from_ref(&file), &[], Compression::None);
    let pack = package.packs.first().unwrap();
    let server = ChunkedServer::start(Arc::clone(pack), always_extra).await;
    let api = MockApi::with_links(Links::remote(&server.url), vec![pack.len() as u64]);
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("game");
    let options = DownloadOptions {
        initial_connections: 1,
        min_connections: 1,
        max_connections: 1,
        adaptive: false,
        client: ClientOptions {
            use_system_proxy: false,
            ..ClientOptions::default()
        },
        ..DownloadOptions::default()
    };
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        install::install(
            &target,
            Arc::new(package.release()),
            Arc::clone(&api),
            &options,
            &DownloadControl::new(None),
        ),
    )
    .await
    .expect("range validation must terminate");

    assert_eq!(server.requests.load(Ordering::Relaxed), 2);
    if always_extra {
        assert!(matches!(
            result,
            Err(InstallError::Download(DownloadError::Integrity {
                pack: 0,
                chunk: 0,
                ..
            }))
        ));
        assert_eq!(api.reports().len(), 1);
        assert_eq!(
            std::fs::read(target.join("data.bin")).unwrap(),
            vec![0; 1024]
        );
        assert_eq!(
            install::read_record(&target).unwrap().unwrap().state,
            InstallState::Installing
        );
    } else {
        assert!(matches!(
            result.unwrap().outcome,
            InstallOutcome::Installed(_)
        ));
        assert!(api.reports().is_empty());
        assert_eq!(
            std::fs::read(target.join("data.bin")).unwrap(),
            file.bytes()
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trailing_body_bytes_retry_before_the_last_chunk_is_written() {
    trailing_bytes_case(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_trailing_body_bytes_report_integrity_and_fail() {
    trailing_bytes_case(true).await;
}
