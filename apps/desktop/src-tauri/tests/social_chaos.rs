//! A4-T12 chaos: launchers keep chatting through the **real** API (in process, fresh PostgreSQL
//! database) while it misbehaves: two API instances behind different sockets, an API restart
//! while a message is being written, and a database connection reset (failover). No message is
//! lost or duplicated.
//!
//! Needs PostgreSQL 18 (or any version with `uuidv7()`):
//! `VGAMES_TEST_DATABASE_URL=postgres://vgames:vgames-dev-only@127.0.0.1:5432/postgres \
//!  cargo test -p vgames-desktop --test social_chaos -- --ignored`

#![allow(
    dead_code, // tests/support/chat.rs is shared; each test uses part of it
    unused_imports,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

include!("support/chat.rs");

impl Launcher {
    async fn connection(&mut self, pick: impl Fn(SocialConnectionState) -> bool) {
        self.wait(|s| matches!(s, Seen::Connection(c) if pick(c.state)))
            .await;
    }

    /// Every text of `conversation` appears exactly once, in this order.
    async fn assert_texts(&self, conversation: Uuid, want: &[&str]) {
        let got: Vec<String> = self
            .texts(conversation)
            .await
            .into_iter()
            .map(|(_, t)| t)
            .collect();
        assert_eq!(got, want, "{}: conversation history", self.name);
    }
}

async fn pair(api: &Api, alice_base: &str, bob_base: &str) -> (Launcher, Launcher, Uuid) {
    let alice_id = api.user("alice").await;
    let bob_id = api.user("bob").await;
    api.befriend(alice_id, bob_id).await;
    let alice = launcher_at(api, alice_base, "alice", alice_id).await;
    let bob = launcher_at(api, bob_base, "bob", bob_id).await;
    alice.device().await;
    bob.device().await;
    let conv = alice
        .service
        .conversation_open_direct(bob_id)
        .await
        .unwrap();
    (alice, bob, conv.id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs PostgreSQL: set VGAMES_TEST_DATABASE_URL"]
async fn two_api_instances_relay_between_their_sockets() {
    let api = start_api().await;
    let (second_base, second) = api.another_instance().await;
    // Alice's socket is on the first instance, Bob's on the second: realtime events cross
    // instances through the database.
    let (mut alice, mut bob, conv) = pair(&api, &api.base.clone(), &second_base).await;
    alice.send(conv, "from instance one").await;
    bob.received_text("from instance one").await;
    bob.send(conv, "from instance two").await;
    alice.received_text("from instance two").await;
    alice.service.typing_start(conv).unwrap();
    bob.wait(|s| matches!(s, Seen::Typing(c, _) if *c == conv))
        .await;
    bob.assert_texts(conv, &["from instance one", "from instance two"])
        .await;

    alice.shutdown.cancel();
    bob.shutdown.cancel();
    second.shutdown.cancel();
    api.stop();
    api.drop_database().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs PostgreSQL: set VGAMES_TEST_DATABASE_URL"]
async fn api_restarts_and_database_resets_lose_no_message() {
    let mut api = start_api().await;
    let base = api.base.clone();
    let (mut alice, mut bob, conv) = pair(&api, &base, &base).await;
    alice.send(conv, "before").await;
    bob.received_text("before").await;

    // The API restarts (deploy): sockets close with 1012, both launchers reconnect by
    // themselves. A message written meanwhile waits in the outbox.
    api.stop();
    alice
        .connection(|s| s == SocialConnectionState::Reconnecting)
        .await;
    bob.connection(|s| s == SocialConnectionState::Reconnecting)
        .await;
    let pending = alice
        .service
        .message_send(conv, "while you were away".into())
        .await
        .unwrap();
    assert_eq!(pending.status, MessageStatus::Pending);
    api.restart().await;
    alice
        .connection(|s| s == SocialConnectionState::Connected)
        .await;
    let id = pending.id;
    alice
        .wait(|s| matches!(s, Seen::Status(i, MessageStatus::Sent) if *i == id))
        .await;
    bob.received_text("while you were away").await;

    // The database drops every connection (failover): the API's LISTEN connection is gone,
    // and whatever it would have announced meanwhile is lost. The API asks its clients to
    // resync, so both launchers reconnect and nothing stays undelivered.
    let killed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM (SELECT pg_terminate_backend(pid) FROM pg_stat_activity
          WHERE datname = $1 AND pid <> pg_backend_pid()) t",
    )
    .bind(&api.db_name)
    .fetch_one(&api.admin)
    .await
    .unwrap();
    assert!(killed > 0);
    alice
        .connection(|s| s == SocialConnectionState::Reconnecting)
        .await;
    bob.connection(|s| s == SocialConnectionState::Reconnecting)
        .await;
    alice
        .connection(|s| s == SocialConnectionState::Connected)
        .await;
    bob.connection(|s| s == SocialConnectionState::Connected)
        .await;
    alice.send(conv, "after the failover").await;
    bob.received_text("after the failover").await;
    bob.send(conv, "all good").await;
    alice.received_text("all good").await;

    let history = [
        "before",
        "while you were away",
        "after the failover",
        "all good",
    ];
    alice.assert_texts(conv, &history).await;
    bob.assert_texts(conv, &history).await;

    alice.shutdown.cancel();
    bob.shutdown.cancel();
    api.stop();
    api.drop_database().await;
}
