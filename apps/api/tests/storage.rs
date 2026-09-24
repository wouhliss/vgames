//! A1-T07: object storage conformance. The same suite runs against the `fs` backend (always)
//! and the GCS emulator (`STORAGE_EMULATOR_HOST` + GCS config; otherwise ignored).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::time::Duration;

use bytes::Bytes;
use futures_util::TryStreamExt;
use reqwest::StatusCode;
use sqlx::PgPool;
use vgames_api::storage::{BucketKind, ByteRange, SignedRequest, Storage};

const TTL: Duration = Duration::from_secs(300);

/// Starts a real server so signed fs URLs resolve; returns the state.
async fn fs_server(pool: PgPool) -> vgames_api::AppState {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let origin = format!("http://127.0.0.1:{port}");
    let config = common::config_with(&[("VGAMES_PUBLIC_URL", &origin)]);
    let state = vgames_api::AppState::new(config, pool).unwrap();
    tokio::spawn(vgames_api::server::serve_on(listener, state.clone()));
    state
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

fn request(c: &reqwest::Client, s: &SignedRequest) -> reqwest::RequestBuilder {
    let method = reqwest::Method::from_bytes(s.method.as_bytes()).unwrap();
    let mut rb = c.request(method, &s.url);
    for (k, v) in &s.headers {
        rb = rb.header(k, v);
    }
    rb
}

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 31 % 251) as u8).collect()
}

async fn read_all(storage: &Storage, name: &str, range: Option<ByteRange>) -> Vec<u8> {
    let s = storage
        .get_range_stream(BucketKind::Packages, name, range)
        .await
        .unwrap();
    let chunks: Vec<Bytes> = s.try_collect().await.unwrap();
    chunks.concat()
}

async fn conformance(storage: &Storage) {
    let c = client();
    let prefix = format!("v1/test-{}", uuid::Uuid::now_v7());

    // Single PUT of an exact length, then signed GET with and without Range.
    let name = format!("{prefix}/manifest.json");
    let data = payload(10_000);
    let put = storage
        .sign_put(
            BucketKind::Packages,
            &name,
            TTL,
            "application/json",
            data.len() as u64,
        )
        .await
        .unwrap();
    assert_eq!(put.method, "PUT");
    let resp = request(&c, &put).body(data.clone()).send().await.unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    assert_eq!(
        storage
            .head(BucketKind::Packages, &name)
            .await
            .unwrap()
            .unwrap()
            .size,
        10_000
    );

    // PUT with a length range: anything in 1..=8192 bytes, nothing else.
    let ranged = format!("{prefix}/ranged.json");
    let put = storage
        .sign_put_range(
            BucketKind::Packages,
            &ranged,
            TTL,
            "application/json",
            1,
            8192,
        )
        .await
        .unwrap();
    for (len, ok) in [(0usize, false), (8193, false), (5000, true)] {
        let resp = request(&c, &put).body(payload(len)).send().await.unwrap();
        assert_eq!(
            resp.status().is_success(),
            ok,
            "{len} bytes: {}",
            resp.status()
        );
    }
    let meta = storage
        .head(BucketKind::Packages, &ranged)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(meta.size, 5000);
    storage.delete(BucketKind::Packages, &ranged).await.unwrap();

    let get = storage
        .sign_get(BucketKind::Packages, &name, TTL)
        .await
        .unwrap();
    let full = request(&c, &get).send().await.unwrap();
    assert_eq!(full.status(), StatusCode::OK);
    assert_eq!(full.bytes().await.unwrap().as_ref(), &data[..]);
    let part = request(&c, &get)
        .header("range", "bytes=100-199")
        .send()
        .await
        .unwrap();
    assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(part.headers()["content-range"], "bytes 100-199/10000");
    assert_eq!(part.bytes().await.unwrap().as_ref(), &data[100..200]);
    assert_eq!(
        read_all(
            storage,
            &name,
            Some(ByteRange {
                start: 9_990,
                end: 20_000
            })
        )
        .await,
        &data[9_990..]
    );

    // Resumable upload, interrupted and resumed.
    let pack = format!("{prefix}/packs/00000.pack");
    let body = payload(700_000);
    let start = storage
        .sign_resumable_start(
            BucketKind::Packages,
            &pack,
            TTL,
            "application/octet-stream",
            1,
            1_000_000,
        )
        .await
        .unwrap();
    assert_eq!(start.method, "POST");
    let resp = request(&c, &start).send().await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "{:?}",
        resp.text().await
    );
    let session = resp.headers()["location"].to_str().unwrap().to_string();

    let first = 262_144 * 2;
    let resp = c
        .put(&session)
        .header("content-range", format!("bytes 0-{}/*", first - 1))
        .body(body[..first].to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 308);
    // "Connection dropped": ask where we are, then resume from there.
    let resp = c
        .put(&session)
        .header("content-range", "bytes */*")
        .body(Vec::new())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 308);
    assert_eq!(resp.headers()["range"], format!("bytes=0-{}", first - 1));
    let resp = c
        .put(&session)
        .header(
            "content-range",
            format!("bytes {first}-{}/{}", body.len() - 1, body.len()),
        )
        .body(body[first..].to_vec())
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    assert_eq!(
        storage
            .head(BucketKind::Packages, &pack)
            .await
            .unwrap()
            .unwrap()
            .size,
        700_000
    );
    assert_eq!(read_all(storage, &pack, None).await, body);

    // Small server-side writes, listing and deletion.
    storage
        .put_small(
            BucketKind::Packages,
            &format!("{prefix}/manifest.sig"),
            Bytes::from_static(b"{}"),
            "application/json",
        )
        .await
        .unwrap();
    let listed = storage
        .list_prefix(BucketKind::Packages, &format!("{prefix}/"))
        .await
        .unwrap();
    assert_eq!(
        listed,
        vec![
            format!("{prefix}/manifest.json"),
            format!("{prefix}/manifest.sig"),
            pack.clone()
        ]
    );
    storage.delete(BucketKind::Packages, &name).await.unwrap();
    storage.delete(BucketKind::Packages, &name).await.unwrap(); // idempotent
    assert!(
        storage
            .head(BucketKind::Packages, &name)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        storage
            .head(BucketKind::Packages, &format!("{prefix}/nope"))
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn fs_backend_conforms(pool: PgPool) {
    let state = fs_server(pool).await;
    conformance(&state.storage).await;
}

#[tokio::test]
#[ignore = "needs STORAGE_EMULATOR_HOST and GCS_BUCKET_* pointing at the compose emulator"]
async fn gcs_backend_conforms() {
    let config = common::config_with(&[
        ("VGAMES_STORAGE_BACKEND", "gcs"),
        (
            "GCS_BUCKET_PACKAGES",
            &std::env::var("GCS_BUCKET_PACKAGES").unwrap_or("vgames-packages".into()),
        ),
        ("GCS_BUCKET_SAVES", "vgames-saves"),
        ("GCS_BUCKET_ASSETS", "vgames-assets"),
        (
            "STORAGE_EMULATOR_HOST",
            &std::env::var("STORAGE_EMULATOR_HOST").expect("STORAGE_EMULATOR_HOST"),
        ),
    ]);
    let storage = Storage::from_config(&config).await.unwrap();
    conformance(&storage).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn fs_urls_enforce_their_signature(pool: PgPool) {
    let state = fs_server(pool).await;
    let storage = &state.storage;
    let c = client();
    let name = "v1/x/object.bin";
    let put = storage
        .sign_put(
            BucketKind::Packages,
            name,
            TTL,
            "application/octet-stream",
            4,
        )
        .await
        .unwrap();

    // Wrong length, wrong content type, tampered token, other object.
    let r = request(&c, &put).body(vec![1u8; 5]).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    let r = c
        .put(&put.url)
        .header("content-type", "text/plain")
        .body(vec![1u8; 4])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let tampered = put.url.replace("t=", "t=x");
    let r = c
        .put(&tampered)
        .header("content-type", "application/octet-stream")
        .body(vec![1u8; 4])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let other = put.url.replace("object.bin", "other.bin");
    let r = c
        .put(&other)
        .header("content-type", "application/octet-stream")
        .body(vec![1u8; 4])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    assert!(
        storage
            .head(BucketKind::Packages, name)
            .await
            .unwrap()
            .is_none(),
        "nothing was written"
    );

    // A GET token cannot be used to PUT.
    request(&c, &put).body(vec![1u8; 4]).send().await.unwrap();
    let get = storage
        .sign_get(BucketKind::Packages, name, TTL)
        .await
        .unwrap();
    let r = c
        .put(&get.url)
        .header("content-type", "application/octet-stream")
        .body(vec![2u8; 4])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);

    // Expired URLs are refused.
    let expired = storage
        .sign_get(BucketKind::Packages, name, Duration::from_secs(0))
        .await
        .unwrap();
    assert_eq!(
        c.get(&expired.url).send().await.unwrap().status(),
        StatusCode::FORBIDDEN
    );

    // Unsatisfiable range.
    let r = c
        .get(&get.url)
        .header("range", "bytes=10-20")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::RANGE_NOT_SATISFIABLE);

    // Resumable sessions refuse gaps and oversize objects.
    let start = storage
        .sign_resumable_start(
            BucketKind::Packages,
            "v1/x/p.pack",
            TTL,
            "application/octet-stream",
            1,
            100,
        )
        .await
        .unwrap();
    let r = c
        .post(&start.url)
        .header("content-type", "application/octet-stream")
        .header("x-goog-resumable", "start")
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::FORBIDDEN,
        "the signed length range header is required"
    );
    let session = request(&c, &start).send().await.unwrap().headers()["location"]
        .to_str()
        .unwrap()
        .to_string();
    let r = c
        .put(&session)
        .header("content-range", "bytes 10-19/*")
        .body(vec![0u8; 10])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    let r = c
        .put(&session)
        .header("content-range", "bytes 0-199/200")
        .body(vec![0u8; 200])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "./migrations")]
async fn traversal_is_impossible(pool: PgPool) {
    let state = fs_server(pool).await;
    for bad in ["../secret", "v1/../../etc/passwd", "/abs", "a//b"] {
        assert!(
            state
                .storage
                .sign_get(BucketKind::Packages, bad, TTL)
                .await
                .is_err(),
            "{bad}"
        );
        assert!(
            state.storage.head(BucketKind::Packages, bad).await.is_err(),
            "{bad}"
        );
    }
    // Even a validly signed request cannot address outside the bucket through the URL.
    let good = state
        .storage
        .sign_get(BucketKind::Packages, "v1/ok", TTL)
        .await
        .unwrap();
    let evil = good
        .url
        .replace("/v1/ok", "/v1/%2e%2e/%2e%2e/%2e%2e/etc/passwd");
    let r = client().get(&evil).send().await.unwrap();
    assert!(r.status().is_client_error(), "{}", r.status());
}
