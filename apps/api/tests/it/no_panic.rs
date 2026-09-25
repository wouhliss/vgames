//! A1-T16: the multipart and query-string extractors (and the image pipeline behind the
//! upload) never panic or fail with a 5xx on arbitrary input; errors stay problem+json.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use crate::common;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use common::*;
use proptest::{
    prelude::*,
    strategy::{Strategy, ValueTree},
    test_runner::TestRunner,
};
use sqlx::PgPool;

const CASES: usize = 200;

/// A small valid PNG, as a seed for mutations.
fn png() -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([10, 20, 30, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

#[derive(Debug, Clone)]
struct Part {
    name: String,
    filename: Option<String>,
    content_type: Option<String>,
    data: Vec<u8>,
}

fn part() -> impl Strategy<Value = Part> {
    let seed = png();
    let data = prop_oneof![
        prop::collection::vec(any::<u8>(), 0..512),
        // Valid PNGs with a few bytes flipped or cut short.
        (
            prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..6),
            any::<prop::sample::Index>()
        )
            .prop_map(move |(flips, cut)| {
                let mut d = seed.clone();
                for (i, b) in flips {
                    let at = i.index(d.len());
                    d[at] ^= b;
                }
                let keep = cut.index(d.len() + 1);
                d.truncate(keep.max(8));
                d
            }),
        "[a-z]{0,20}".prop_map(String::into_bytes),
    ];
    (
        prop::sample::select(vec!["file", "kind", "image", "x", ""]).prop_map(str::to_string),
        prop::option::of("[a-zA-Z0-9._/\\\\-]{0,30}"),
        prop::option::of(
            prop::sample::select(vec![
                "image/png",
                "image/jpeg",
                "image/webp",
                "image/gif",
                "text/plain",
                "application/octet-stream",
                "",
            ])
            .prop_map(str::to_string),
        ),
        data,
    )
        .prop_map(|(name, filename, content_type, data)| Part {
            name,
            filename,
            content_type,
            data,
        })
}

fn body(parts: &[Part], boundary: &str, kind: Option<&str>) -> Vec<u8> {
    let mut b = Vec::new();
    if let Some(k) = kind {
        b.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"kind\"\r\n\r\n{k}\r\n")
                .as_bytes(),
        );
    }
    for p in parts {
        b.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"",
                p.name
            )
            .as_bytes(),
        );
        if let Some(f) = &p.filename {
            b.extend_from_slice(format!("; filename=\"{f}\"").as_bytes());
        }
        b.extend_from_slice(b"\r\n");
        if let Some(ct) = &p.content_type {
            b.extend_from_slice(format!("Content-Type: {ct}\r\n").as_bytes());
        }
        b.extend_from_slice(b"\r\n");
        b.extend_from_slice(&p.data);
        b.extend_from_slice(b"\r\n");
    }
    b.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    b
}

async fn check(app: &Router, req: Request<Body>, what: &str) {
    let resp = send(app, req).await;
    let status = resp.status();
    assert!(!status.is_server_error(), "{what}: {status}");
    if status.is_client_error() {
        assert_eq!(
            resp.headers()["content-type"],
            "application/problem+json",
            "{what}: {status}"
        );
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn multipart_uploads_never_fail_with_5xx(pool: PgPool) {
    let app = app(pool.clone());
    let (_, _, admin) = seed_session(&pool, "admin").await;
    let resp = send(
        &app,
        common::publishing::json_req(
            "POST",
            "/v1/admin/packages",
            &admin,
            &serde_json::json!({ "title": "Fuzz", "fetch_metadata": false }),
        ),
    )
    .await;
    let id = body_json(resp).await["id"].as_str().unwrap().to_string();
    let uri = format!("/v1/admin/packages/{id}/assets");

    let strategy = (
        prop::collection::vec(part(), 0..4),
        prop::option::of(prop::sample::select(vec![
            "cover",
            "hero",
            "logo",
            "screenshot",
            "icon",
            "banner",
            "",
        ])),
        "[A-Za-z0-9'()+_,./:=?-]{1,70}",
        prop::sample::select(vec![
            "multipart/form-data; boundary=",
            "multipart/form-data; boundary=\"",
            "multipart/mixed; boundary=",
            "multipart/form-data;boundary=",
        ]),
        any::<bool>(),
    );
    let mut runner = TestRunner::deterministic();
    for _ in 0..CASES {
        let (parts, kind, boundary, ct, truncate) =
            strategy.new_tree(&mut runner).unwrap().current();
        let mut data = body(&parts, &boundary, kind);
        if truncate {
            data.truncate(data.len() / 2);
        }
        let req = Request::builder()
            .method("POST")
            .uri(&uri)
            .header("authorization", format!("Bearer {admin}"))
            .header("content-type", format!("{ct}{boundary}"))
            .body(Body::from(data))
            .unwrap();
        check(&app, req, &format!("{} parts, kind {kind:?}", parts.len())).await;
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn query_strings_never_fail_with_5xx(pool: PgPool) {
    let app = app(pool.clone());
    let (_, _, owner) = seed_session(&pool, "owner").await;
    let doc = serde_json::to_value(vgames_api::http::openapi()).unwrap();
    let mut gets = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        let Some(op) = item.get("get") else { continue };
        let names: Vec<String> = op["parameters"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["in"] == "query")
            .map(|p| p["name"].as_str().unwrap().to_string())
            .collect();
        if !names.is_empty() && !path.contains('{') {
            gets.push((path.clone(), names));
        }
    }
    assert!(gets.len() >= 5, "{gets:?}");

    let mut runner = TestRunner::deterministic();
    for (path, names) in &gets {
        let mut keys = names.clone();
        keys.push("unknown".into());
        let pair = (
            prop::sample::select(keys),
            prop_oneof![
                "[a-zA-Z0-9_.~-]{0,30}",
                "(%[0-9A-Fa-f]{2}){0,10}",
                "%[0-9A-Fa-f]?",
                "-?[0-9]{1,25}",
                Just("2026-09-25T10:00:00Z".to_string()),
                Just("01920000-0000-7000-8000-000000000001".to_string()),
            ],
        );
        let strategy = prop::collection::vec(pair, 0..5);
        for _ in 0..40 {
            let pairs = strategy.new_tree(&mut runner).unwrap().current();
            let query: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect();
            let uri = format!("{path}?{}", query.join("&"));
            let req = Request::builder()
                .uri(&uri)
                .header("authorization", format!("Bearer {owner}"))
                .body(Body::empty())
                .unwrap();
            check(&app, req, &uri).await;
        }
    }
    // A request that is not even valid UTF-8 after decoding.
    let resp = send(&app, bearer_request("GET", "/v1/packages?q=%FF%FE", &owner)).await;
    assert_ne!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
