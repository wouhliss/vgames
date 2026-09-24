//! Metadata fetching and image downloads (A1-T10), against wiremock providers.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::{collections::HashMap, num::NonZeroU32, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use vgames_api::{
    AppState,
    config::IgdbConfig,
    jobs::{self, Registry},
    metadata::{
        Endpoints, ProviderRates, Providers,
        safe_fetch::{FetchPolicy, IMAGE_HOSTS, is_public},
    },
    secret::Secret,
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, method, path, query_param},
};

fn fixture(name: &str) -> Value {
    let p = format!(
        "{}/tests/fixtures/metadata/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// Providers pointed at `server`; image hosts resolve to it too, over plain HTTP.
fn providers(server: &MockServer, overrides: &[(&str, &str)]) -> Providers {
    let mock = *server.address();
    let mut resolve: HashMap<String, Vec<std::net::SocketAddr>> = IMAGE_HOSTS
        .iter()
        .map(|h| ((*h).to_string(), vec![mock]))
        .collect();
    for (host, addr) in overrides {
        resolve.insert((*host).to_string(), vec![addr.parse().unwrap()]);
    }
    let policy = FetchPolicy {
        https_only: false,
        timeout: Duration::from_secs(5),
        // Loopback is where the mock lives; every other private range stays blocked.
        address_allowed: |ip| ip.is_loopback() || is_public(ip),
        resolve_overrides: resolve,
        ..FetchPolicy::production()
    };
    let fast = NonZeroU32::new(1000).unwrap();
    Providers::new(
        Some(IgdbConfig {
            client_id: "test-client".into(),
            client_secret: Secret::new("test-secret".into()),
        }),
        true,
        Endpoints::all_at(&server.uri()),
        ProviderRates {
            igdb: fast,
            steam: fast,
        },
        policy,
    )
    .unwrap()
}

async fn setup(pool: &PgPool, server: &MockServer) -> (AppState, Router, String) {
    let state = state(pool.clone())
        .with_metadata(providers(server, &[]))
        .ok()
        .unwrap();
    let app = vgames_api::http::router(state.clone());
    let (_, _, admin) = seed_session(pool, "admin").await;
    (state, app, admin)
}

fn authed(method: &str, uri: &str, token: &str, body: Option<&Value>) -> Request<Body> {
    let b = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"));
    match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(v).unwrap()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

async fn create(app: &Router, token: &str, body: Value) -> Value {
    let resp = send(
        app,
        authed("POST", "/v1/admin/packages", token, Some(&body)),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    body_json(resp).await
}

async fn get_admin(app: &Router, token: &str, id: &str) -> (Value, String) {
    let resp = send(
        app,
        authed("GET", &format!("/v1/admin/packages/{id}"), token, None),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let etag = resp
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    (body_json(resp).await, etag)
}

/// Runs queued jobs until none is ready.
async fn drain(state: &AppState) -> usize {
    let registry = Registry::standard();
    let mut n = 0;
    while jobs::run_one(state, &registry, "test").await.unwrap() {
        n += 1;
        assert!(n < 50, "job loop");
    }
    n
}

async fn mount_token(server: &MockServer, times: u64) {
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .and(body_string_contains("grant_type=client_credentials"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "fake-token", "expires_in": 5_000_000, "token_type": "bearer"
        })))
        .expect(times)
        .mount(server)
        .await;
}

async fn mount_celeste(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/v4/games"))
        .and(body_string_contains("search \"celeste\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("igdb_search_celeste.json")))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/storesearch/"))
        .and(query_param("term", "celeste"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture("steam_storesearch_celeste.json")),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/appdetails"))
        .and(query_param("appids", "504230"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture("steam_appdetails_504230.json")),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/reports/summaries/504230.json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "tier": "platinum", "total": 1200 })),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/umu_api.php"))
        .and(query_param("codename", "504230"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{ "title": "Celeste", "umu_id": "umu-504230", "store": "steam", "codename": "504230" }])),
        )
        .mount(server)
        .await;
}

async fn images_payload(pool: &PgPool) -> Option<Value> {
    sqlx::query_scalar(
        "SELECT payload FROM jobs WHERE kind = 'metadata.images' ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn unique_match_is_applied_and_admin_fields_are_kept(pool: PgPool) {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    mount_celeste(&server).await;
    let (state, app, admin) = setup(&pool, &server).await;

    let pkg = create(&app, &admin, json!({ "title": "celeste" })).await;
    let id = pkg["id"].as_str().unwrap().to_string();
    assert_eq!(pkg["metadata_job"]["state"], "queued");
    drain(&state).await;

    let (pkg, _) = get_admin(&app, &admin, &id).await;
    assert_eq!(pkg["metadata_job"]["state"], "succeeded");
    // IGDB wins where both providers have a value; the admin's title stays.
    assert_eq!(pkg["title"], "celeste");
    assert_eq!(pkg["field_sources"]["title"], "admin");
    assert_eq!(pkg["developer"], "Maddy Makes Games");
    assert_eq!(pkg["release_date"], "2018-01-25");
    assert_eq!(pkg["genres"], json!(["Platform", "Adventure", "Indie"]));
    assert_eq!(pkg["igdb_id"], 26226);
    assert_eq!(pkg["steam_app_id"], 504230);
    assert_eq!(pkg["protondb_tier"], "platinum");
    assert_eq!(pkg["field_sources"]["summary"], "igdb");
    assert!(pkg["summary"].as_str().unwrap().chars().count() <= 500);

    let c = body_json(
        send(
            &app,
            authed(
                "GET",
                &format!("/v1/admin/packages/{id}/metadata/candidates"),
                &admin,
                None,
            ),
        )
        .await,
    )
    .await;
    let items = c["items"].as_array().unwrap();
    let keys: Vec<(String, i64)> = items
        .iter()
        .map(|i| {
            (
                i["source"].as_str().unwrap().to_string(),
                i["external_id"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        &keys[..2],
        [("igdb".to_string(), 26226), ("steam".to_string(), 504230)]
    );
    assert!(
        keys.contains(&("igdb".to_string(), 119133)),
        "weaker matches are kept for the admin"
    );
    assert!(
        !keys.iter().any(|k| k.1 == 800000),
        "irrelevant search results are dropped"
    );
    let steam = &items[1]["data"];
    assert_eq!(steam["external"]["umu_id"], "umu-504230");
    let description = steam["description"].as_str().unwrap();
    assert!(
        !description.contains('<') && !description.contains("alert"),
        "{description}"
    );
    assert!(description.contains("- 700+ screens of hardcore platforming"));
    let classic = items.iter().find(|i| i["external_id"] == 119133).unwrap();
    assert_eq!(
        classic["data"]["summary"],
        "The original PICO-8 prototype & jam game."
    );

    // Images: IGDB cover/hero/screenshots, Steam logo (IGDB has none); all allowlisted HTTPS.
    let payload = images_payload(&pool).await.unwrap();
    let images = payload["images"].as_array().unwrap();
    let by_kind = |k: &str| images.iter().filter(|i| i["kind"] == k).collect::<Vec<_>>();
    assert_eq!(
        by_kind("cover")[0]["url"],
        "https://images.igdb.com/igdb/image/upload/t_cover_big_2x/co3byy.jpg"
    );
    assert_eq!(by_kind("logo")[0]["source"], "steam");
    assert_eq!(by_kind("screenshot").len(), 2);

    // A refresh reuses the cached Twitch token (expect(1) above) and replaces candidates.
    let resp = send(
        &app,
        authed(
            "POST",
            &format!("/v1/admin/packages/{id}/metadata/refresh"),
            &admin,
            None,
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let job = body_json(resp).await;
    let again = body_json(
        send(
            &app,
            authed(
                "POST",
                &format!("/v1/admin/packages/{id}/metadata/refresh"),
                &admin,
                None,
            ),
        )
        .await,
    )
    .await;
    assert_eq!(
        job["id"], again["id"],
        "refresh is deduplicated while queued"
    );
    drain(&state).await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM metadata_candidates")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 3);
}

#[sqlx::test(migrations = "./migrations")]
async fn ambiguous_titles_are_left_to_the_admin(pool: PgPool) {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("POST"))
        .and(path("/v4/games"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("igdb_search_doom.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/storesearch/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "total": 0, "items": [] })))
        .mount(&server)
        .await;
    let (state, app, admin) = setup(&pool, &server).await;
    let id = create(&app, &admin, json!({ "title": "Doom" })).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    drain(&state).await;

    let (pkg, etag) = get_admin(&app, &admin, &id).await;
    assert_eq!(pkg["metadata_job"]["state"], "succeeded");
    assert!(pkg.get("igdb_id").is_none_or(Value::is_null), "{pkg}");
    assert!(pkg["genres"].as_array().unwrap().is_empty());
    assert!(images_payload(&pool).await.is_none());
    let c = body_json(
        send(
            &app,
            authed(
                "GET",
                &format!("/v1/admin/packages/{id}/metadata/candidates"),
                &admin,
                None,
            ),
        )
        .await,
    )
    .await;
    assert_eq!(c["items"].as_array().unwrap().len(), 2);

    // The admin picks the 2016 game.
    let apply = |etag: Option<&str>, body: Value| {
        let mut r = authed(
            "POST",
            &format!("/v1/admin/packages/{id}/metadata/apply"),
            &admin,
            Some(&body),
        );
        if let Some(e) = etag {
            r.headers_mut().insert(header::IF_MATCH, e.parse().unwrap());
        }
        r
    };
    let pick = json!({ "source": "igdb", "external_id": 7351, "fields": ["title", "genres", "external_ids", "cover"] });
    assert_eq!(
        send(&app, apply(None, pick.clone())).await.status(),
        StatusCode::PRECONDITION_REQUIRED
    );
    assert_eq!(
        send(&app, apply(Some("W/\"1\""), pick.clone()))
            .await
            .status(),
        StatusCode::PRECONDITION_FAILED
    );
    let unknown = json!({ "source": "steam", "external_id": 7351, "fields": ["title"] });
    let resp = send(&app, apply(Some(&etag), unknown)).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(resp).await["errors"][0]["code"],
        "unknown_candidate"
    );
    let empty = json!({ "source": "igdb", "external_id": 7351, "fields": [] });
    assert_eq!(
        send(&app, apply(Some(&etag), empty)).await.status(),
        StatusCode::BAD_REQUEST
    );

    let resp = send(&app, apply(Some(&etag), pick.clone())).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let new_etag = resp
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let pkg = body_json(resp).await;
    assert_eq!(
        pkg["title"], "Doom",
        "admin title is kept without overwrite_admin_fields"
    );
    assert_eq!(pkg["genres"], json!(["Shooter"]));
    assert_eq!(pkg["igdb_id"], 7351);
    assert_eq!(pkg["steam_app_id"], 379720);
    assert_eq!(pkg["field_sources"]["genres"], "igdb");
    assert_eq!(
        images_payload(&pool).await.unwrap()["images"][0]["kind"],
        "cover"
    );

    let overwrite = json!({ "source": "igdb", "external_id": 7351, "fields": ["title"], "overwrite_admin_fields": true });
    let pkg = body_json(send(&app, apply(Some(&new_etag), overwrite)).await).await;
    assert_eq!(pkg["title"], "DOOM");
    assert_eq!(pkg["field_sources"]["title"], "igdb");
}

#[sqlx::test(migrations = "./migrations")]
async fn explicit_steam_id_is_used_even_when_titles_differ(pool: PgPool) {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    mount_celeste(&server).await;
    Mock::given(method("POST"))
        .and(path("/v4/games"))
        .and(body_string_contains("external_games.uid = \"504230\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("igdb_search_celeste.json")))
        .mount(&server)
        .await;
    let (state, app, admin) = setup(&pool, &server).await;
    let id = create(
        &app,
        &admin,
        json!({ "title": "My Mountain Game", "steam_app_id": 504230 }),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    drain(&state).await;
    let (pkg, _) = get_admin(&app, &admin, &id).await;
    // Both providers are looked up by the Steam id, not the (unrelated) title.
    assert_eq!(pkg["developer"], "Maddy Makes Games");
    assert_eq!(pkg["igdb_id"], 26226);
    assert_eq!(pkg["title"], "My Mountain Game");
    assert_eq!(pkg["field_sources"]["steam_app_id"], "admin");
    assert_eq!(pkg["protondb_tier"], "platinum");
}

#[sqlx::test(migrations = "./migrations")]
async fn provider_outage_retries_then_fails_without_blocking_the_package(pool: PgPool) {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("POST"))
        .and(path("/v4/games"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let (state, app, admin) = setup(&pool, &server).await;
    let id = create(&app, &admin, json!({ "title": "Outage" })).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    for attempt in 1..=5 {
        sqlx::query("UPDATE jobs SET run_at = now() WHERE kind = 'metadata.fetch'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(drain(&state).await, 1, "attempt {attempt}");
    }
    let (pkg, etag) = get_admin(&app, &admin, &id).await;
    assert_eq!(pkg["metadata_job"]["state"], "dead");
    assert_eq!(pkg["metadata_job"]["attempts"], 5);
    assert!(
        pkg["metadata_job"]["last_error"]
            .as_str()
            .unwrap()
            .contains("500")
    );
    // The package is untouched and still editable.
    let mut req = authed(
        "PATCH",
        &format!("/v1/admin/packages/{id}"),
        &admin,
        Some(&json!({ "summary": "Manual" })),
    );
    req.headers_mut()
        .insert(header::IF_MATCH, etag.parse().unwrap());
    assert_eq!(send(&app, req).await.status(), StatusCode::OK);
}

fn png(rgb: [u8; 3]) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(40, 20, image::Rgb(rgb));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

#[sqlx::test(migrations = "./migrations")]
async fn images_are_downloaded_under_the_ssrf_rules(pool: PgPool) {
    let server = MockServer::start().await;
    let loopback = format!("http://127.0.0.1:{}", server.address().port());
    for (p, rgb) in [("/cover.png", [10, 120, 200]), ("/shot.png", [200, 10, 10])] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(png(rgb))
                    .insert_header("content-type", "image/png"),
            )
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/same-host-redirect.png"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/shot.png"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/to-loopback.png"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{loopback}/cover.png").as_str()),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/huge.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 10 * 1024 * 1024 + 1]))
        .mount(&server)
        .await;

    // `cdn.akamai.steamstatic.com` resolves to a private address.
    let providers = providers(&server, &[("cdn.akamai.steamstatic.com", "10.0.0.7:80")]);
    let state = state(pool.clone()).with_metadata(providers).ok().unwrap();
    let app = vgames_api::http::router(state.clone());
    let (_, _, admin) = seed_session(&pool, "admin").await;
    let id = create(
        &app,
        &admin,
        json!({ "title": "Pictures", "fetch_metadata": false }),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let data = json!({
        "title": "Pictures",
        "images": {
            "cover": "http://images.igdb.com/cover.png",
            "hero": "http://images.igdb.com/to-loopback.png",
            "logo": "http://evil.example/logo.png",
            "screenshots": [
                "http://images.igdb.com/same-host-redirect.png",
                "http://cdn.akamai.steamstatic.com/private.png",
                "http://images.igdb.com/huge.png",
                format!("{loopback}/shot.png"),
            ]
        }
    });
    sqlx::query(
        "INSERT INTO metadata_candidates (package_id, source, external_id, title, score, data) VALUES ($1, 'igdb', 5, 'Pictures', 1, $2)",
    )
    .bind(id.parse::<Uuid>().unwrap())
    .bind(&data)
    .execute(&pool)
    .await
    .unwrap();
    let (_, etag) = get_admin(&app, &admin, &id).await;
    let mut req = authed(
        "POST",
        &format!("/v1/admin/packages/{id}/metadata/apply"),
        &admin,
        Some(
            &json!({ "source": "igdb", "external_id": 5, "fields": ["cover", "hero", "logo", "screenshots"] }),
        ),
    );
    req.headers_mut()
        .insert(header::IF_MATCH, etag.parse().unwrap());
    assert_eq!(send(&app, req).await.status(), StatusCode::OK);
    assert_eq!(drain(&state).await, 1);

    let job_state: String =
        sqlx::query_scalar("SELECT state FROM jobs WHERE kind = 'metadata.images'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        job_state, "succeeded",
        "blocked images are skipped, not retried"
    );
    let assets: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT kind, source_url FROM package_assets ORDER BY kind, source_url")
            .fetch_all(&pool)
            .await
            .unwrap();
    // Stored: the cover and the screenshot behind a same-host redirect. Refused: a redirect to
    // 127.0.0.1, a host off the allowlist, a host resolving to 10.x, an IP literal, > 10 MiB.
    assert_eq!(
        assets,
        [
            (
                "cover".to_string(),
                Some("http://images.igdb.com/cover.png".to_string())
            ),
            (
                "screenshot".to_string(),
                Some("http://images.igdb.com/same-host-redirect.png".to_string())
            ),
        ]
    );
    let (pkg, _) = get_admin(&app, &admin, &id).await;
    assert_eq!(pkg["field_sources"]["cover"], "igdb");
    assert!(pkg["cover"]["id"].is_string());
    assert!(pkg.get("hero").is_none_or(Value::is_null));
    assert!(pkg.get("logo").is_none_or(Value::is_null));
}
