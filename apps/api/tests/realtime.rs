//! A1-T05: realtime gateway across two API instances sharing one database.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::{net::SocketAddr, time::Duration};

use axum::http::StatusCode;
use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_tungstenite::tungstenite::{Message, protocol::frame::coding::CloseCode};
use vgames_api::{
    AppState,
    realtime::{self, bus, hub::Target},
};

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Instance {
    state: AppState,
    addr: SocketAddr,
}

async fn instance(pool: &PgPool) -> Instance {
    let state = common::state(pool.clone());
    realtime::start(&state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(vgames_api::server::serve_on(listener, state.clone()));
    let mut ready = state.realtime_ready.subscribe();
    tokio::time::timeout(Duration::from_secs(10), ready.wait_for(|r| *r))
        .await
        .unwrap()
        .unwrap();
    Instance { state, addr }
}

async fn ticket(inst: &Instance, token: &str) -> String {
    let resp = send(
        &vgames_api::http::router(inst.state.clone()),
        bearer_request("POST", "/v1/realtime/ticket", token),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    body_json(resp).await["ticket"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn connect(
    inst: &Instance,
    ticket: &str,
) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    let url = format!("ws://{}/v1/realtime?ticket={ticket}", inst.addr);
    tokio_tungstenite::connect_async(url)
        .await
        .map(|(ws, _)| ws)
}

/// Next text frame as JSON (skips pings).
async fn next_event(ws: &mut Ws) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("event in time")
            .expect("open")
            .unwrap();
        match msg {
            Message::Text(t) => return serde_json::from_str(t.as_str()).unwrap(),
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("unexpected frame {other:?}"),
        }
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn events_published_on_one_instance_reach_sockets_on_another(pool: PgPool) {
    let a = instance(&pool).await;
    let b = instance(&pool).await;
    let (user, session, token) = seed_session(&pool, "user").await;

    // Ticket issued by A, redeemed on B.
    let t = ticket(&a, &token).await;
    let mut ws = connect(&b, &t).await.unwrap();
    let hello = next_event(&mut ws).await;
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["v"], 1);
    assert_eq!(hello["data"]["user_id"], user.to_string());
    assert!(b.state.realtime.is_connected(user));

    bus::publish(
        &a.state.db,
        &Target::users(&[user]),
        "invite.created",
        json!({"n": 1}),
    )
    .await
    .unwrap();
    let ev = next_event(&mut ws).await;
    assert_eq!(
        (ev["type"].as_str(), ev["data"]["n"].as_i64()),
        (Some("invite.created"), Some(1))
    );
    assert!(ev["id"].is_string() && ev["ts"].is_string());

    // Payloads above the NOTIFY limit travel by reference.
    let big = "x".repeat(20_000);
    bus::publish(
        &a.state.db,
        &Target::users(&[user]),
        "big.event",
        json!({"blob": big}),
    )
    .await
    .unwrap();
    let ev = next_event(&mut ws).await;
    assert_eq!(ev["data"]["blob"].as_str().unwrap().len(), 20_000);

    // Events inside a rolled-back transaction are never delivered.
    let mut tx = pool.begin().await.unwrap();
    bus::publish_tx(&mut tx, &Target::users(&[user]), "never", json!({}))
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    bus::publish(&a.state.db, &Target::users(&[user]), "after", json!({}))
        .await
        .unwrap();
    assert_eq!(next_event(&mut ws).await["type"], "after");

    // Revoking the session sends session.revoked, then closes with 4001.
    bus::revoke_sessions(&a.state, user, &[session], "logout")
        .await
        .unwrap();
    assert_eq!(next_event(&mut ws).await["type"], "session.revoked");
    match tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await
        .unwrap()
    {
        Some(Ok(Message::Close(Some(frame)))) => assert_eq!(frame.code, CloseCode::from(4001)),
        other => panic!("expected close, got {other:?}"),
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn tickets_are_single_use_and_bound_to_live_sessions(pool: PgPool) {
    let a = instance(&pool).await;
    let (_, session, token) = seed_session(&pool, "user").await;

    let t = ticket(&a, &token).await;
    let _ws = connect(&a, &t).await.unwrap();
    let err = connect(&a, &t).await.expect_err("reuse must fail");
    assert!(err.to_string().contains("401"), "{err}");

    let garbage = connect(&a, "not-a-ticket")
        .await
        .expect_err("unknown ticket");
    assert!(garbage.to_string().contains("401"), "{garbage}");

    let t2 = ticket(&a, &token).await;
    sqlx::query("UPDATE sessions SET revoked_at = now(), revoked_reason = 'logout' WHERE id = $1")
        .bind(session)
        .execute(&pool)
        .await
        .unwrap();
    let revoked = connect(&a, &t2).await.expect_err("revoked session");
    assert!(revoked.to_string().contains("401"), "{revoked}");

    // Tickets need credentials.
    let resp = send(
        &vgames_api::http::router(a.state.clone()),
        bearer_request("POST", "/v1/realtime/ticket", "vga_nope"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "./migrations")]
async fn ping_unknown_events_and_shutdown(pool: PgPool) {
    let a = instance(&pool).await;
    let (_, _, token) = seed_session(&pool, "user").await;
    let mut ws = connect(&a, &ticket(&a, &token).await).await.unwrap();
    assert_eq!(next_event(&mut ws).await["type"], "hello");

    ws.send(Message::Text(r#"{"v":1,"type":"ping","data":null}"#.into()))
        .await
        .unwrap();
    assert_eq!(next_event(&mut ws).await["type"], "pong");
    ws.send(Message::Text(r#"{"v":1,"type":"nope","data":{}}"#.into()))
        .await
        .unwrap();
    let err = next_event(&mut ws).await;
    assert_eq!(
        (err["type"].as_str(), err["data"]["code"].as_str()),
        (Some("error"), Some("unknown_event"))
    );
    ws.send(Message::Text("garbage".into())).await.unwrap();
    assert_eq!(next_event(&mut ws).await["data"]["code"], "invalid_frame");

    a.state.shutdown.cancel();
    match tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await
        .unwrap()
    {
        Some(Ok(Message::Close(Some(frame)))) => assert_eq!(frame.code, CloseCode::Restart),
        other => panic!("expected 1012 close, got {other:?}"),
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn plain_http_requests_to_the_socket_are_rejected(pool: PgPool) {
    let resp = send(&app(pool), get_req("/v1/realtime?ticket=abc")).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(resp).await["code"], "websocket_required");
}
