//! INS-02 acceptance tests against a mock server: paging with cursors, genre
//! filter, offline, 401 → one refresh then `unauthenticated`, packages that are
//! gone, no release for this host, covers (hit, miss, refused), and hard
//! blockers from a compat provider.

use std::sync::Arc;

use serde_json::{Value, json};
use url::Url;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::compat::{CompatProfile, CompatProvider, CompatQuery, ProfileFuture};
use super::*;
use crate::api::Session;
use crate::images::{ImageCache, ImageKey, MAX_IMAGE_BYTES};
use crate::secrets::{MemoryVault, VaultHandle};

const SERVER: &str = "01920000-0000-7000-8000-000000000001";
const PACKAGE: &str = "01920000-0000-7000-8000-0000000000a1";
const COVER: &str = "01920000-0000-7000-8000-0000000000c1";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake-cover";

fn server_id() -> Uuid {
    Uuid::parse_str(SERVER).unwrap()
}

fn package_id() -> Uuid {
    Uuid::parse_str(PACKAGE).unwrap()
}

struct TestConnections {
    client: ApiClient,
}

impl Connections for TestConnections {
    async fn active(&self) -> Result<ApiClient, CatalogError> {
        Ok(self.client.clone())
    }
    async fn client(&self, _server_id: Uuid) -> Result<ApiClient, CatalogError> {
        Ok(self.client.clone())
    }
}

struct Harness {
    mock: MockServer,
    catalog: Catalog<TestConnections>,
    db: Db,
    _dir: tempfile::TempDir,
}

fn tokens(access: &str) -> vgames_proto::auth::TokenResponse {
    serde_json::from_value(json!({
        "access_token": access,
        "refresh_token": format!("refresh-{access}"),
        "token_type": "Bearer",
        "expires_in": 900,
        "user": {
            "id": "01920000-0000-7000-8000-00000000000a",
            "username": "alice",
            "role": "user",
            "created_at": "2026-09-24T10:00:00Z"
        }
    }))
    .unwrap()
}

impl Harness {
    async fn new(host: Host) -> Self {
        let mock = MockServer::start().await;
        Self::at(mock.uri().parse().unwrap(), mock, host).await
    }

    async fn at(base: Url, mock: MockServer, host: Host) -> Self {
        let session = Arc::new(Session::new(
            server_id(),
            base.clone(),
            VaultHandle(Arc::new(MemoryVault::default())),
            Arc::new(|_| {}),
        ));
        session.store(&tokens("access-1")).await;
        let client = ApiClient::new(crate::api::http_client().unwrap(), base, session);
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(ImageCache::open(dir.path().join("images")).unwrap());
        let covers = Arc::new(Covers::new(cache, crate::api::http_client().unwrap()));
        let db = Db::open_in_memory().unwrap();
        let catalog = Catalog::new(
            Arc::new(TestConnections { client }),
            db.clone(),
            covers,
            Some(host),
        );
        Self {
            mock,
            catalog,
            db,
            _dir: dir,
        }
    }

    async fn requests_to(&self, route: &str) -> Vec<Request> {
        self.mock
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path() == route)
            .collect()
    }

    async fn serve_detail(&self, detail: Value) {
        Mock::given(method("GET"))
            .and(path(format!("/v1/packages/{PACKAGE}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(detail))
            .mount(&self.mock)
            .await;
    }
}

fn summary(id: &str, title: &str, platforms: &[&str]) -> Value {
    json!({
        "id": id,
        "slug": title.to_lowercase().replace(' ', "-"),
        "title": title,
        "genres": ["Action"],
        "cover": {
            "id": COVER, "kind": "cover", "url": format!("/v1/assets/{COVER}"),
            "width": 600, "height": 900, "content_type": "image/png"
        },
        "platforms": platforms,
        "updated_at": "2026-10-01T12:00:00Z"
    })
}

fn release(platform: &str, sequence: i64) -> Value {
    json!({
        "platform": platform,
        "version_id": format!("01920000-0000-7000-8000-0000000001{sequence:02}"),
        "version_label": format!("1.{sequence}"),
        "sequence": sequence,
        "total_size": 1_000_000,
        "published_at": "2026-10-01T12:00:00Z"
    })
}

fn detail(platforms: &[&str]) -> Value {
    let mut value = summary(PACKAGE, "Gilded Garden", platforms);
    value["description"] = json!("A *garden*.");
    value["release_date"] = json!("2026-03-05");
    value["protondb_tier"] = json!("gold");
    value["releases"] = Value::Array(platforms.iter().map(|p| release(p, 3)).collect());
    value
}

fn query(cursor: Option<&str>) -> CatalogQuery {
    CatalogQuery {
        query: "  garden ".into(),
        genre: Some("Action".into()),
        sort: CatalogSort::Recent,
        cursor: cursor.map(Into::into),
    }
}

#[tokio::test]
async fn pages_follow_cursors_and_cache_until_refreshed() {
    let h = Harness::new(Host::LinuxX86_64).await;
    Mock::given(method("GET"))
        .and(path("/v1/packages"))
        .and(query_param_is_missing("cursor"))
        .and(query_param("q", "garden"))
        .and(query_param("genre", "Action"))
        .and(query_param("sort", "recent"))
        .and(query_param("limit", "48"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [summary(PACKAGE, "Gilded Garden", &["linux-x86_64"])],
            "next_cursor": "c2"
        })))
        .mount(&h.mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/packages"))
        .and(query_param("cursor", "c2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [summary("01920000-0000-7000-8000-0000000000a2", "Second", &["windows-x86_64"])]
        })))
        .mount(&h.mock)
        .await;

    let first = h.catalog.list(query(None)).await.unwrap();
    assert_eq!(first.next_cursor.as_deref(), Some("c2"));
    let item = &first.items[0];
    assert_eq!(item.availability, Availability::Native);
    assert_eq!(item.platforms, vec![types::Platform::LinuxX86_64]);
    assert_eq!(item.updated_at, "2026-10-01T12:00:00Z");
    assert!(item.cover_url.as_deref().unwrap().contains("vgimg"));

    let second = h.catalog.list(query(Some("c2"))).await.unwrap();
    assert_eq!(second.next_cursor, None);
    assert_eq!(second.items[0].availability, Availability::Proton);
    // Later pages come from the cache; a first page is a refresh.
    h.catalog.list(query(Some("c2"))).await.unwrap();
    assert_eq!(h.requests_to("/v1/packages").await.len(), 2);
    h.catalog.list(query(None)).await.unwrap();
    h.catalog.list(query(Some("c2"))).await.unwrap();
    assert_eq!(h.requests_to("/v1/packages").await.len(), 4);
    // An explicit invalidation (server switch, sign-out) drops everything.
    h.catalog.invalidate();
    h.catalog.list(query(Some("c2"))).await.unwrap();
    assert_eq!(h.requests_to("/v1/packages").await.len(), 5);
}

#[tokio::test]
async fn genres_are_listed_with_counts() {
    let h = Harness::new(Host::LinuxX86_64).await;
    Mock::given(method("GET"))
        .and(path("/v1/genres"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"genre": "Action", "count": 3}, {"genre": "Puzzle", "count": 1}]
        })))
        .mount(&h.mock)
        .await;
    assert_eq!(
        h.catalog.genres().await.unwrap(),
        vec![
            GenreCount {
                genre: "Action".into(),
                count: 3
            },
            GenreCount {
                genre: "Puzzle".into(),
                count: 1
            }
        ]
    );
}

#[tokio::test]
async fn invalid_queries_never_reach_the_server() {
    let h = Harness::new(Host::LinuxX86_64).await;
    let mut long = query(None);
    long.query = "x".repeat(101);
    assert!(matches!(
        h.catalog.list(long).await,
        Err(CatalogError::Server { code, .. }) if code == "invalid_query"
    ));
    assert!(h.requests_to("/v1/packages").await.is_empty());
}

#[tokio::test]
async fn an_unreachable_server_is_offline() {
    let mock = MockServer::start().await;
    // Nothing listens on port 9 (discard) on the test machines.
    let h = Harness::at(
        "http://127.0.0.1:9/".parse().unwrap(),
        mock,
        Host::LinuxX86_64,
    )
    .await;
    assert_eq!(
        h.catalog.list(query(None)).await,
        Err(CatalogError::Offline)
    );
    assert_eq!(
        h.catalog.plan(package_id()).await.unwrap_err(),
        InstallPlanError::Offline
    );
}

#[tokio::test]
async fn a_401_refreshes_once_then_reports_unauthenticated() {
    let h = Harness::new(Host::LinuxX86_64).await;
    Mock::given(method("GET"))
        .and(path("/v1/packages"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"code": "unauthenticated"})))
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access-2", "refresh_token": "refresh-2", "token_type": "Bearer",
            "expires_in": 900,
            "user": {"id": "01920000-0000-7000-8000-00000000000a", "username": "alice",
                     "role": "user", "created_at": "2026-09-24T10:00:00Z"}
        })))
        .mount(&h.mock)
        .await;
    assert_eq!(
        h.catalog.list(query(None)).await,
        Err(CatalogError::Unauthenticated)
    );
    assert_eq!(h.requests_to("/v1/auth/token").await.len(), 1);
    let listed = h.requests_to("/v1/packages").await;
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[1].headers["authorization"], "Bearer access-2");
}

#[tokio::test]
async fn missing_or_hidden_packages_are_not_found() {
    let h = Harness::new(Host::LinuxX86_64).await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/packages/{PACKAGE}")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"code": "not_found"})))
        .mount(&h.mock)
        .await;
    assert_eq!(
        h.catalog.details(package_id()).await,
        Err(CatalogError::NotFound)
    );
    assert_eq!(
        h.catalog.plan(package_id()).await.unwrap_err(),
        InstallPlanError::NotFound
    );
}

#[tokio::test]
async fn no_release_for_this_host() {
    let h = Harness::new(Host::LinuxX86_64).await;
    h.serve_detail(detail(&["macos-aarch64"])).await;
    let details = h.catalog.details(package_id()).await.unwrap();
    assert_eq!(details.release, None);
    assert_eq!(details.compat, CompatInfo::Unavailable);
    assert_eq!(
        h.catalog.plan(package_id()).await.unwrap_err(),
        InstallPlanError::NoRelease
    );
}

#[tokio::test]
async fn details_pick_the_hosts_release_and_default_compat() {
    let h = Harness::new(Host::LinuxX86_64).await;
    h.serve_detail(detail(&["windows-x86_64", "macos-aarch64"]))
        .await;
    let details = h.catalog.details(package_id()).await.unwrap();
    assert_eq!(details.release_date.as_deref(), Some("2026-03-05"));
    let release = details.release.unwrap();
    assert_eq!(release.platform, types::Platform::WindowsX86_64);
    assert_eq!(release.via, Availability::Proton);
    assert_eq!(
        details.compat,
        CompatInfo::Compat {
            layer: CompatRunner::Proton,
            status: CompatStatus::Untested,
            notes: None,
            protondb_tier: Some(ProtonDbTier::Gold),
            blockers: vec![],
        }
    );
    let plan = h.catalog.plan(package_id()).await.unwrap().plan;
    assert_eq!(plan.download_bytes, 1_000_000);
    assert_eq!(plan.required_bytes, 1_000_000 + 64 * 1024 * 1024);
    // The plan reused the details just fetched.
    assert_eq!(
        h.requests_to(&format!("/v1/packages/{PACKAGE}"))
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn installed_packages_cannot_be_planned_again() {
    let h = Harness::new(Host::LinuxX86_64).await;
    h.serve_detail(detail(&["linux-x86_64"])).await;
    h.db.call(|conn| {
        conn.execute(
            "INSERT INTO servers (id, url, name, root_public_key, root_fingerprint, added_at)
             VALUES (?1, 'https://example.test', 'Test', zeroblob(32), 'VG1', 0)",
            [SERVER],
        )?;
        conn.execute(
            "INSERT INTO libraries (id, path, label, created_at) VALUES ('lib', '/games', 'Games', 0)",
            [],
        )?;
        conn.execute(
            "INSERT INTO installs (server_id, package_id, library_id, dir_name, version_id,
             sequence, platform, state)
             VALUES (?1, ?2, 'lib', 'game', 'v', 1, 'linux-x86_64', 'installed')",
            [SERVER, PACKAGE],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        h.catalog.plan(package_id()).await.unwrap_err(),
        InstallPlanError::AlreadyInstalled
    );
}

struct FakeProvider(Vec<CompatBlocker>);

impl CompatProvider for FakeProvider {
    fn profile(&self, query: CompatQuery) -> ProfileFuture<'_> {
        assert_eq!(query.package.package_id, package_id());
        let blockers = self.0.clone();
        Box::pin(async move {
            Some(CompatProfile {
                status: CompatStatus::Playable,
                notes: Some("Works with a controller.".into()),
                blockers,
            })
        })
    }
}

#[tokio::test]
async fn a_hard_blocker_from_the_provider_blocks_the_install() {
    let h = Harness::new(Host::MacosAarch64).await;
    h.serve_detail(detail(&["windows-x86_64"])).await;
    h.catalog.set_compat_provider(Arc::new(FakeProvider(vec![
        CompatBlocker::D3d12UnsupportedOnMac,
    ])));
    let details = h.catalog.details(package_id()).await.unwrap();
    let CompatInfo::Compat {
        layer,
        status,
        protondb_tier,
        ..
    } = &details.compat
    else {
        panic!("expected a compat layer, got {:?}", details.compat);
    };
    assert_eq!(*layer, CompatRunner::Wine);
    assert_eq!(*status, CompatStatus::Playable);
    // ProtonDB is a Proton rating; Wine routes never show it.
    assert_eq!(*protondb_tier, None);
    assert_eq!(
        h.catalog.plan(package_id()).await.unwrap_err(),
        InstallPlanError::Blocked {
            blocker: CompatBlocker::D3d12UnsupportedOnMac
        }
    );

    h.catalog
        .set_compat_provider(Arc::new(FakeProvider(vec![CompatBlocker::NeedsRosetta])));
    assert!(matches!(
        h.catalog.plan(package_id()).await,
        Err(InstallPlanError::Blocked {
            blocker: CompatBlocker::NeedsRosetta
        })
    ));

    // A sunset warning informs; it never blocks.
    h.catalog
        .set_compat_provider(Arc::new(FakeProvider(vec![CompatBlocker::RosettaSunset {
            last_macos: "27".into(),
        }])));
    h.catalog.plan(package_id()).await.unwrap();
}

fn cover_key() -> ImageKey {
    ImageKey::for_asset(server_id(), COVER).unwrap()
}

async fn serve_cover(h: &Harness, body: Vec<u8>, content_type: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/v1/assets/{COVER}")))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/signed/cover?sig=abc", h.mock.uri())),
        )
        .mount(&h.mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/signed/cover"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", content_type)
                .set_body_bytes(body),
        )
        .mount(&h.mock)
        .await;
}

#[tokio::test]
async fn covers_are_fetched_once_then_served_from_the_cache() {
    let h = Harness::new(Host::LinuxX86_64).await;
    h.serve_detail(detail(&["linux-x86_64"])).await;
    serve_cover(&h, PNG.to_vec(), "image/png").await;
    let covers = h.catalog.covers();
    let connections = h.catalog.connections().as_ref();

    // Unknown to the catalog: never fetched.
    assert_eq!(covers.load(connections, cover_key()).await.unwrap(), None);
    assert!(
        h.requests_to(&format!("/v1/assets/{COVER}"))
            .await
            .is_empty()
    );

    let details = h.catalog.details(package_id()).await.unwrap();
    assert_eq!(details.cover_url, Some(cover_key().url()));
    let image = covers
        .load(connections, cover_key())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(image.bytes, PNG);
    assert_eq!(image.content_type, "image/png");
    let again = covers
        .load(connections, cover_key())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.bytes, PNG);
    assert_eq!(h.requests_to(&format!("/v1/assets/{COVER}")).await.len(), 1);
    // The bearer token stays with the API; the signed URL gets none.
    let signed = h.requests_to("/signed/cover").await;
    assert_eq!(signed.len(), 1);
    assert!(!signed[0].headers.contains_key("authorization"));
}

#[tokio::test]
async fn oversized_or_non_image_covers_are_refused() {
    for (body, content_type) in [
        (vec![0x89; MAX_IMAGE_BYTES + 1], "image/png"),
        (b"<html>not an image</html>".to_vec(), "image/png"),
        (PNG.to_vec(), "text/html"),
    ] {
        let h = Harness::new(Host::LinuxX86_64).await;
        h.serve_detail(detail(&["linux-x86_64"])).await;
        serve_cover(&h, body, content_type).await;
        h.catalog.details(package_id()).await.unwrap();
        let covers = h.catalog.covers();
        let connections = h.catalog.connections().as_ref();
        assert!(
            covers.load(connections, cover_key()).await.is_err(),
            "{content_type}"
        );
        // Nothing was cached: the next request asks again.
        assert!(covers.load(connections, cover_key()).await.is_err());
        assert_eq!(h.requests_to(&format!("/v1/assets/{COVER}")).await.len(), 2);
    }
}
