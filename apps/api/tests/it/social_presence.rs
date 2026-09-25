//! A4-T03: presence (05-social §3, 05-social-notes §2.4) over real sockets on two instances.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use crate::common::{self, publishing::json_req, *};

use std::{net::SocketAddr, time::Duration};

use axum::http::StatusCode;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;
use vgames_api::{
    AppState,
    realtime::{self, bus, hub::Target},
    social::presence,
};

pub(crate) type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub(crate) struct Instance {
    pub(crate) state: AppState,
    addr: SocketAddr,
}

pub(crate) async fn instance(pool: &PgPool) -> Instance {
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

impl Instance {
    pub(crate) fn app(&self) -> axum::Router {
        vgames_api::http::router(self.state.clone())
    }

    pub(crate) async fn connect(&self, token: &str) -> Ws {
        let resp = send(
            &self.app(),
            bearer_request("POST", "/v1/realtime/ticket", token),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let ticket = body_json(resp).await["ticket"]
            .as_str()
            .unwrap()
            .to_string();
        let url = format!("ws://{}/v1/realtime?ticket={ticket}", self.addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        assert_eq!(next_event(&mut ws).await["type"], "hello");
        ws
    }

    async fn put_presence(&self, token: &str, body: Value) -> StatusCode {
        send(&self.app(), json_req("PUT", "/v1/presence", token, &body))
            .await
            .status()
    }
}

/// Next text frame as JSON (skips pings).
pub(crate) async fn next_event(ws: &mut Ws) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("event in time")
            .expect("open")
            .unwrap();
        if let Message::Text(t) = msg {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

/// Asserts that `ws` received nothing before a marker event published now.
pub(crate) async fn assert_quiet(state: &AppState, ws: &mut Ws, user: Uuid) {
    bus::publish(&state.db, &Target::users(&[user]), "test.marker", json!({}))
        .await
        .unwrap();
    let ev = next_event(ws).await;
    assert_eq!(ev["type"], "test.marker", "unexpected event {ev}");
}

pub(crate) async fn befriend(pool: &PgPool, a: Uuid, b: Uuid, state: &str) {
    let (low, high) = if a < b { (a, b) } else { (b, a) };
    sqlx::query(
        "INSERT INTO friendships (user_low, user_high, state, requested_by, accepted_at)
         VALUES ($1, $2, $3, $4, CASE WHEN $3 = 'accepted' THEN now() END)",
    )
    .bind(low)
    .bind(high)
    .bind(state)
    .bind(a)
    .execute(pool)
    .await
    .unwrap();
}

async fn package(pool: &PgPool, slug: &str, status: &str, by: Uuid) -> Uuid {
    sqlx::query_scalar("INSERT INTO packages (slug, title, status, created_by) VALUES ($1, $2, $3, $4) RETURNING id")
        .bind(slug)
        .bind(format!("Title of {slug}"))
        .bind(status)
        .bind(by)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn backdate_heartbeat(pool: &PgPool, user: Uuid, secs: i32) {
    sqlx::query("UPDATE user_presence SET heartbeat_at = now() - make_interval(secs => $2) WHERE user_id = $1")
        .bind(user)
        .bind(f64::from(secs))
        .execute(pool)
        .await
        .unwrap();
}

async fn wait_disconnected(state: &AppState, user: Uuid) {
    for _ in 0..200 {
        if !state.realtime.is_connected(user) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("socket still registered");
}

#[sqlx::test(migrations = "./migrations")]
async fn presence_reaches_accepted_friends_only(pool: PgPool) {
    let inst = instance(&pool).await;
    let (a, _, ta) = seed_session(&pool, "user").await;
    let (b, _, tb) = seed_session(&pool, "user").await;
    let (c, _, tc) = seed_session(&pool, "user").await;
    let (d, _, td) = seed_session(&pool, "user").await;
    befriend(&pool, a, b, "accepted").await;
    befriend(&pool, c, a, "pending").await;
    let game = package(&pool, "coop-quest", "published", a).await;
    let hidden = package(&pool, "secret", "hidden", a).await;
    let (mut wb, mut wc, mut wd) = (
        inst.connect(&tb).await,
        inst.connect(&tc).await,
        inst.connect(&td).await,
    );

    assert_eq!(
        inst.put_presence(&ta, json!({"status": "in_game", "package_id": game}))
            .await,
        StatusCode::NO_CONTENT
    );
    let ev = next_event(&mut wb).await;
    assert_eq!(ev["type"], "presence.changed");
    assert_eq!(
        ev["data"],
        json!({"user_id": a, "status": "in_game", "package_id": game, "package_title": "Title of coop-quest"})
    );
    // Pending friends and strangers hear nothing.
    assert_quiet(&inst.state, &mut wc, c).await;
    assert_quiet(&inst.state, &mut wd, d).await;

    // Unchanged presence publishes nothing.
    inst.put_presence(&ta, json!({"status": "in_game", "package_id": game}))
        .await;
    assert_quiet(&inst.state, &mut wb, b).await;

    // A package that is not published is never named.
    inst.put_presence(&ta, json!({"status": "in_game", "package_id": hidden}))
        .await;
    assert_eq!(
        next_event(&mut wb).await["data"],
        json!({"user_id": a, "status": "in_game"})
    );

    // The friend list carries presence for accepted friends only.
    let resp = send(&inst.app(), bearer_request("GET", "/v1/friends", &tb)).await;
    let list = body_json(resp).await;
    assert_eq!(list["friends"][0]["presence"]["status"], "in_game");
    assert!(
        list["friends"][0]["presence"]
            .get("package_title")
            .is_none()
    );
    let resp = send(&inst.app(), bearer_request("GET", "/v1/friends", &tc)).await;
    assert!(
        body_json(resp).await["outgoing"][0]
            .get("presence")
            .is_none()
    );

    // After a block, presence stops in both directions.
    let resp = send(
        &inst.app(),
        bearer_request("POST", &format!("/v1/blocks/{a}"), &tb),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(next_event(&mut wb).await["type"], "friend.removed");
    inst.put_presence(&ta, json!({"status": "online"})).await;
    assert_quiet(&inst.state, &mut wb, b).await;

    // Validation.
    for bad in [
        json!({"status": "online", "package_id": game}),
        json!({"status": "offline"}),
        json!({"status": "online", "extra": 1}),
    ] {
        assert_eq!(
            inst.put_presence(&ta, bad.clone()).await,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn socket_presence_heartbeats_and_goes_offline_after_disconnect(pool: PgPool) {
    let one = instance(&pool).await;
    let two = instance(&pool).await;
    let (a, _, ta) = seed_session(&pool, "user").await;
    let (b, _, tb) = seed_session(&pool, "user").await;
    befriend(&pool, a, b, "accepted").await;
    let mut wb = one.connect(&tb).await;
    let mut wa = two.connect(&ta).await;

    // presence.set over the socket on instance two reaches B's socket on instance one.
    wa.send(Message::Text(
        r#"{"v":1,"type":"presence.set","data":{"status":"away"}}"#.into(),
    ))
    .await
    .unwrap();
    let ev = next_event(&mut wb).await;
    assert_eq!(
        (ev["type"].as_str(), ev["data"]["status"].as_str()),
        (Some("presence.changed"), Some("away"))
    );
    // An invalid presence.set is answered on the socket and changes nothing.
    wa.send(Message::Text(
        r#"{"v":1,"type":"presence.set","data":{"status":"offline"}}"#.into(),
    ))
    .await
    .unwrap();
    let err = next_event(&mut wa).await;
    assert_eq!(err["type"], "error", "{err}");
    assert_quiet(&one.state, &mut wb, b).await;

    // A REST update handled by instance one is claimed by instance two's heartbeat, which
    // holds the socket, and survives the sweep.
    one.put_presence(&ta, json!({"status": "online"})).await;
    assert_eq!(next_event(&mut wb).await["data"]["status"], "online");
    backdate_heartbeat(&pool, a, 31).await;
    assert_eq!(presence::heartbeat(&one.state).await.unwrap(), 0);
    assert_eq!(presence::heartbeat(&two.state).await.unwrap(), 1);
    let owner: String =
        sqlx::query_scalar("SELECT instance_id FROM user_presence WHERE user_id = $1")
            .bind(a)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(owner, two.state.instance_id);
    assert!(
        presence::sweep_stale(&one.state, presence::OFFLINE_AFTER)
            .await
            .unwrap()
            .is_empty()
    );
    let seen: Option<time::OffsetDateTime> =
        sqlx::query_scalar("SELECT last_seen_at FROM users WHERE id = $1")
            .bind(a)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(seen.is_some());

    // After the socket closes nothing refreshes the heartbeat, and 30 s later A is offline,
    // published exactly once.
    wa.close(None).await.unwrap();
    wait_disconnected(&two.state, a).await;
    assert_eq!(presence::heartbeat(&two.state).await.unwrap(), 0);
    backdate_heartbeat(&pool, a, 29).await;
    vgames_api::social::sweep_once(&one.state).await.unwrap();
    assert_quiet(&one.state, &mut wb, b).await;
    backdate_heartbeat(&pool, a, 31).await;
    vgames_api::social::sweep_once(&one.state).await.unwrap();
    assert_eq!(
        next_event(&mut wb).await["data"],
        json!({"user_id": a, "status": "offline"})
    );
    vgames_api::social::sweep_once(&two.state).await.unwrap();
    assert_quiet(&one.state, &mut wb, b).await;

    let resp = send(&one.app(), bearer_request("GET", "/v1/friends", &tb)).await;
    assert_eq!(
        body_json(resp).await["friends"][0]["presence"]["status"],
        "offline"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn rest_presence_without_a_socket_lapses(pool: PgPool) {
    let inst = instance(&pool).await;
    let (a, _, ta) = seed_session(&pool, "user").await;
    let (b, _, tb) = seed_session(&pool, "user").await;
    befriend(&pool, a, b, "accepted").await;
    let mut wb = inst.connect(&tb).await;

    inst.put_presence(&ta, json!({"status": "online"})).await;
    assert_eq!(next_event(&mut wb).await["data"]["status"], "online");
    // B's own socket keeps B alive, but A has none.
    presence::heartbeat(&inst.state).await.unwrap();
    backdate_heartbeat(&pool, a, 31).await;
    let gone = presence::sweep_stale(&inst.state, presence::OFFLINE_AFTER)
        .await
        .unwrap();
    assert_eq!(gone, vec![a]);
    assert_eq!(next_event(&mut wb).await["data"]["status"], "offline");
    // Setting presence again brings A back.
    inst.put_presence(&ta, json!({"status": "online"})).await;
    assert_eq!(next_event(&mut wb).await["data"]["status"], "online");
}

#[sqlx::test(migrations = "./migrations")]
async fn presence_reaches_hundreds_of_friends(pool: PgPool) {
    let inst = instance(&pool).await;
    let (a, _, ta) = seed_session(&pool, "user").await;
    let (b, _, tb) = seed_session(&pool, "user").await;
    befriend(&pool, a, b, "accepted").await;
    // 299 more friends: the recipients no longer fit in one NOTIFY payload.
    sqlx::query(
        "WITH u AS (
           INSERT INTO users (discord_id, username) SELECT (720000000000000000 + g)::text, 'filler' FROM generate_series(1, 299) g
           RETURNING id)
         INSERT INTO friendships (user_low, user_high, state, requested_by, accepted_at)
         SELECT least($1, id), greatest($1, id), 'accepted', $1, now() FROM u",
    )
    .bind(a)
    .execute(&pool)
    .await
    .unwrap();
    let mut wb = inst.connect(&tb).await;
    assert_eq!(
        inst.put_presence(&ta, json!({"status": "online"})).await,
        StatusCode::NO_CONTENT
    );
    let ev = next_event(&mut wb).await;
    assert_eq!(
        (ev["type"].as_str(), ev["data"]["user_id"].as_str()),
        (Some("presence.changed"), Some(a.to_string().as_str()))
    );
}
