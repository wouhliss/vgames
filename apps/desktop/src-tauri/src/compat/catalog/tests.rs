use std::io::Cursor;
use std::path::{Path, PathBuf};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

fn vectors() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../crates/vgames-core/tests/vectors/runtimes")
}

fn vector(name: &str) -> Vec<u8> {
    std::fs::read(vectors().join(name)).unwrap()
}

fn vector_text(name: &str) -> String {
    String::from_utf8(vector(name)).unwrap()
}

/// A source for `server` with the vectors' test key (or `key`).
fn source(server: &MockServer, key: Option<&str>) -> CatalogSource {
    let key = key.map_or_else(|| vector_text("runtime-catalog-test.pub"), str::to_owned);
    let url = Url::parse(&format!("{}/runtimes/runtimes.json", server.uri())).unwrap();
    CatalogSource {
        signature_url: Url::parse(&format!("{url}.minisig")).unwrap(),
        url,
        key_id: key_id(&key).unwrap(),
        public_key: key,
    }
}

async fn serve(server: &MockServer, catalog: Vec<u8>, signature: Vec<u8>) {
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/runtimes/runtimes.json"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(catalog))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/runtimes/runtimes.json.minisig"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(signature))
        .mount(server)
        .await;
}

fn manager(source: CatalogSource, db: Db) -> RuntimeCatalog {
    RuntimeCatalog::new(Some(source), db, http_client().unwrap())
}

async fn rows(db: &Db) -> Vec<(String, i64)> {
    db.call(|conn| {
        let mut stmt =
            conn.prepare("SELECT key_id, version FROM compat_catalogs ORDER BY key_id")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a_valid_catalog_is_verified_stored_and_served() {
    let server = MockServer::start().await;
    serve(
        &server,
        vector("runtimes-v2.json"),
        vector("runtimes-v2.json.minisig"),
    )
    .await;
    let db = Db::open_in_memory().unwrap();
    let m = manager(source(&server, None), db.clone());
    assert_eq!(m.status().await, CatalogStatus::Missing);
    let accepted = m.refresh().await.unwrap().unwrap();
    assert_eq!(accepted.catalog.version, 2);
    assert!(matches!(
        m.status().await,
        CatalogStatus::Ready { version: 2, .. }
    ));
    assert_eq!(rows(&db).await, [(source(&server, None).key_id, 2)]);
}

#[tokio::test]
async fn an_older_catalog_after_a_newer_one_is_a_rollback() {
    let server = MockServer::start().await;
    let db = Db::open_in_memory().unwrap();
    let m = manager(source(&server, None), db.clone());
    serve(
        &server,
        vector("runtimes-v2.json"),
        vector("runtimes-v2.json.minisig"),
    )
    .await;
    m.refresh().await.unwrap();
    serve(
        &server,
        vector("runtimes-v1.json"),
        vector("runtimes-v1.json.minisig"),
    )
    .await;
    let err = m.refresh().await.unwrap_err();
    assert!(
        matches!(
            err,
            CatalogError::Invalid(RuntimesError::Rollback { got: 1, seen: 2 })
        ),
        "{err:?}"
    );
    assert_eq!(m.current().await.unwrap().unwrap().catalog.version, 2);
    assert_eq!(
        m.status().await,
        CatalogStatus::Failed {
            reason: err.to_string(),
            stored_version: Some(2)
        }
    );
    assert_eq!(rows(&db).await, [(source(&server, None).key_id, 2)]);
}

#[tokio::test]
async fn tampered_and_foreign_catalogs_are_refused_and_nothing_is_stored() {
    let server = MockServer::start().await;
    let db = Db::open_in_memory().unwrap();
    let m = manager(source(&server, None), db.clone());
    for (catalog, sig) in [
        ("runtimes-v2-tampered.json", "runtimes-v2.json.minisig"),
        ("runtimes-v2.json", "runtimes-v2.json.other-key.minisig"),
    ] {
        serve(&server, vector(catalog), vector(sig)).await;
        let err = m.refresh().await.unwrap_err();
        assert!(
            matches!(err, CatalogError::Invalid(RuntimesError::BadSignature)),
            "{catalog} + {sig}: {err:?}"
        );
    }
    assert!(rows(&db).await.is_empty());
    assert!(m.current().await.unwrap().is_none());
}

#[tokio::test]
async fn oversized_bodies_are_refused_while_reading() {
    let server = MockServer::start().await;
    let db = Db::open_in_memory().unwrap();
    let m = manager(source(&server, None), db.clone());
    serve(
        &server,
        vec![b' '; MAX_CATALOG_BYTES + 1],
        vector("runtimes-v2.json.minisig"),
    )
    .await;
    assert!(matches!(m.refresh().await, Err(CatalogError::TooLarge)));
    serve(
        &server,
        vector("runtimes-v2.json"),
        vec![b'a'; MAX_SIGNATURE_BYTES + 1],
    )
    .await;
    assert!(matches!(m.refresh().await, Err(CatalogError::TooLarge)));
    assert!(rows(&db).await.is_empty());
}

fn sign(key: &minisign::SecretKey, data: &[u8]) -> Vec<u8> {
    minisign::sign(None, key, Cursor::new(data), Some("test"), None)
        .unwrap()
        .to_string()
        .into_bytes()
}

fn with_version(catalog: &[u8], version: u64) -> Vec<u8> {
    let mut json: serde_json::Value = serde_json::from_slice(catalog).unwrap();
    json["version"] = version.into();
    serde_json::to_vec(&json).unwrap()
}

#[tokio::test]
async fn a_high_version_under_one_key_never_blocks_another_key() {
    // F2: key A reached version 9; a catalog at version 2 under key B is still accepted.
    let server = MockServer::start().await;
    let db = Db::open_in_memory().unwrap();
    let a = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let a_pub = a.pk.to_box().unwrap().to_string();
    let v9 = with_version(&vector("runtimes-v2.json"), 9);
    serve(&server, v9.clone(), sign(&a.sk, &v9)).await;
    let under_a = manager(source(&server, Some(&a_pub)), db.clone());
    assert_eq!(under_a.refresh().await.unwrap().unwrap().catalog.version, 9);

    serve(
        &server,
        vector("runtimes-v2.json"),
        vector("runtimes-v2.json.minisig"),
    )
    .await;
    let under_b = manager(source(&server, None), db.clone());
    assert_eq!(under_b.refresh().await.unwrap().unwrap().catalog.version, 2);

    let mut want = vec![
        (key_id(&a_pub).unwrap(), 9),
        (source(&server, None).key_id, 2),
    ];
    want.sort();
    assert_eq!(rows(&db).await, want);

    // And key A's own rollback is still refused.
    let v8 = with_version(&vector("runtimes-v2.json"), 8);
    serve(&server, v8.clone(), sign(&a.sk, &v8)).await;
    assert!(matches!(
        under_a.refresh().await,
        Err(CatalogError::Invalid(RuntimesError::Rollback {
            got: 8,
            seen: 9
        }))
    ));
}

#[tokio::test]
async fn the_catalog_is_fetched_at_most_every_six_hours_and_survives_a_restart() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("launcher.db");
    {
        let db = Db::open(&db_path).unwrap();
        serve(
            &server,
            vector("runtimes-v2.json"),
            vector("runtimes-v2.json.minisig"),
        )
        .await;
        let m = manager(source(&server, None), db.clone());
        m.refresh_if_due().await.unwrap();
        m.refresh_if_due().await.unwrap();
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            2,
            "one fetch (catalog + signature)"
        );
    }
    // Restart: the stored catalog is served offline, verified again.
    server.reset().await;
    let db = Db::open(&db_path).unwrap();
    let m = manager(source(&server, None), db.clone());
    assert_eq!(
        m.refresh_if_due().await.unwrap().unwrap().catalog.version,
        2
    );
    assert!(server.received_requests().await.unwrap().is_empty());

    // Seven hours later the next call fetches again.
    db.call(|conn| {
        conn.execute(
            "UPDATE compat_catalogs SET checked_at = checked_at - 7 * 3600",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    serve(
        &server,
        vector("runtimes-v2.json"),
        vector("runtimes-v2.json.minisig"),
    )
    .await;
    m.refresh_if_due().await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_stored_catalog_that_no_longer_verifies_is_ignored() {
    let server = MockServer::start().await;
    let db = Db::open_in_memory().unwrap();
    serve(
        &server,
        vector("runtimes-v2.json"),
        vector("runtimes-v2.json.minisig"),
    )
    .await;
    manager(source(&server, None), db.clone())
        .refresh()
        .await
        .unwrap();
    let tampered = vector("runtimes-v2-tampered.json");
    db.call(move |conn| {
        conn.execute("UPDATE compat_catalogs SET catalog = ?1", [tampered])?;
        Ok(())
    })
    .await
    .unwrap();
    let fresh = manager(source(&server, None), db.clone());
    assert!(fresh.current().await.unwrap().is_none());
}

#[test]
fn release_builds_ignore_the_overrides() {
    let compiled = vector_text("runtime-catalog-test.pub");
    let other = minisign::KeyPair::generate_unencrypted_keypair()
        .unwrap()
        .pk
        .to_box()
        .unwrap()
        .to_string();
    let env = |name: &str| match name {
        CATALOG_URL_ENV => Some("http://127.0.0.1:9/runtimes.json".to_owned()),
        CATALOG_PUBKEY_ENV => Some(other.clone()),
        _ => None,
    };
    let resolved = CatalogSource::resolve(&compiled, env).unwrap();
    if cfg!(debug_assertions) {
        assert_eq!(resolved.url.as_str(), "http://127.0.0.1:9/runtimes.json");
        assert_eq!(resolved.key_id, key_id(&other).unwrap());
    } else {
        assert_eq!(resolved.url.as_str(), DEFAULT_CATALOG_URL);
        assert_eq!(resolved.key_id, key_id(&compiled).unwrap());
    }
    assert_eq!(
        resolved.signature_url.as_str(),
        format!("{}.minisig", resolved.url)
    );
}

#[tokio::test]
async fn without_a_key_the_manager_is_off() {
    assert_eq!(CatalogSource::resolve("", |_| None), None);
    assert_eq!(CatalogSource::resolve("  \n", |_| None), None);
    assert_eq!(CatalogSource::resolve("not a key", |_| None), None);
    // This build embeds the key exactly when the repository has it.
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../runtimes/runtime-catalog.pub");
    assert_eq!(
        CatalogSource::resolve(COMPILED_KEY, |_| None).is_some(),
        file.exists()
    );
    let m = RuntimeCatalog::new(None, Db::open_in_memory().unwrap(), http_client().unwrap());
    assert_eq!(m.status().await, CatalogStatus::Off);
    assert!(matches!(m.refresh_if_due().await, Err(CatalogError::Off)));
}

#[tokio::test]
async fn an_older_read_never_replaces_a_newer_accepted_catalog() {
    // A reader verified the stored v1 row, then paused; meanwhile v2 was accepted.
    let server = MockServer::start().await;
    let db = Db::open_in_memory().unwrap();
    let m = manager(source(&server, None), db.clone());
    serve(
        &server,
        vector("runtimes-v2.json"),
        vector("runtimes-v2.json.minisig"),
    )
    .await;
    m.refresh().await.unwrap();
    let key = source(&server, None);
    let older = verify_catalog(
        &vector("runtimes-v1.json"),
        &vector_text("runtimes-v1.json.minisig"),
        &key.public_key,
        None,
    )
    .unwrap();
    let published = m.publish(Accepted {
        catalog: Arc::new(older.catalog),
        key_id: key.key_id.clone(),
    });
    assert_eq!(
        published.catalog.version, 2,
        "the paused reader gets the newer catalog"
    );
    assert_eq!(m.current().await.unwrap().unwrap().catalog.version, 2);
}

#[tokio::test]
async fn concurrent_due_checks_fetch_once() {
    let server = MockServer::start().await;
    serve(
        &server,
        vector("runtimes-v2.json"),
        vector("runtimes-v2.json.minisig"),
    )
    .await;
    let m = Arc::new(manager(
        source(&server, None),
        Db::open_in_memory().unwrap(),
    ));
    let calls: Vec<_> = (0..8)
        .map(|_| {
            let m = Arc::clone(&m);
            tokio::spawn(async move { m.refresh_if_due().await.unwrap().unwrap().catalog.version })
        })
        .collect();
    for call in calls {
        assert_eq!(call.await.unwrap(), 2);
    }
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        2,
        "one catalog and one signature request"
    );
    // A forced refresh still fetches.
    m.refresh().await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
}
