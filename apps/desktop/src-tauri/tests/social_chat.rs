//! A4-T08 acceptance: launchers chat end to end through the **real** API (in process, on a
//! fresh PostgreSQL database): Alice and Bob exchange messages (pre-key, then normal Olm
//! messages); a second device of Bob joins and receives new messages but not old ones;
//! revoking it stops delivery to it; the server never holds a plaintext.
//!
//! Needs PostgreSQL 18 (or any version with `uuidv7()`):
//! `VGAMES_TEST_DATABASE_URL=postgres://vgames:vgames-dev-only@127.0.0.1:5432/postgres \
//!  cargo test -p vgames-desktop --test social_chat -- --ignored`

#![allow(
    dead_code, // tests/support/chat.rs is shared; each test uses part of it
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

// Shared with the other real-API tests (the API in process, launchers, event recorder).
include!("support/chat.rs");

// ---- the scenario -------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs PostgreSQL: set VGAMES_TEST_DATABASE_URL"]
async fn two_launchers_chat_and_a_new_device_gets_only_new_messages() {
    let api = start_api().await;
    let alice_id = api.user("alice").await;
    let bob_id = api.user("bob").await;
    api.befriend(alice_id, bob_id).await;

    let mut alice = launcher(&api, "alice", alice_id).await;
    let mut bob = launcher(&api, "bob", bob_id).await;
    let _alice_device = alice.device().await;
    let _bob_device = bob.device().await;

    // Alice opens the direct conversation and writes first (pre-key message).
    let conv = alice
        .service
        .conversation_open_direct(bob_id)
        .await
        .unwrap();
    alice.send(conv.id, "hello bob").await;
    let got = bob.received_text("hello bob").await;
    assert!(!got.mine);
    assert_eq!(got.sender_user_id, alice_id);
    assert_eq!(got.conversation_id, conv.id);

    // Bob answers (normal messages from here on), and both keep talking.
    let bob_convs = bob.service.conversations_list().await.unwrap();
    assert_eq!(bob_convs.len(), 1);
    assert_eq!(bob_convs[0].unread, 1);
    bob.service.conversation_mark_read(conv.id).await.unwrap();
    assert_eq!(bob.service.conversations_list().await.unwrap()[0].unread, 0);
    bob.send(conv.id, "hi alice").await;
    alice.received_text("hi alice").await;
    alice.send(conv.id, "old news").await;
    bob.received_text("old news").await;

    // Typing reaches the other side (and is throttled on the sender).
    alice.service.typing_start(conv.id).unwrap();
    alice.service.typing_start(conv.id).unwrap();
    bob.wait(|s| matches!(s, Seen::Typing(c, u) if *c == conv.id && *u == alice_id))
        .await;

    // Safety numbers agree on both sides.
    let a_sn = alice.service.contact_security(bob_id).await.unwrap();
    let b_sn = bob.service.contact_security(alice_id).await.unwrap();
    assert_eq!(a_sn.safety_number, b_sn.safety_number);
    assert_eq!(a_sn.safety_number.len(), 60);

    // Bob signs in on a second computer.
    let mut bob2 = launcher(&api, "bob2", bob_id).await;
    let bob2_device = bob2.device().await;
    // Alice is told about Bob's new device in the conversation.
    let notice = alice
        .wait(|s| matches!(s, Seen::Notice(_, DeviceNotice::NewDevice { device_id, .. }) if *device_id == bob2_device))
        .await;
    assert!(matches!(notice, Seen::Notice(Some(c), _) if c == conv.id));
    // The safety number changed with Bob's device list.
    let a_sn2 = alice.service.contact_security(bob_id).await.unwrap();
    assert_ne!(a_sn2.safety_number, a_sn.safety_number);

    // New messages reach both of Bob's devices; Bob's own messages reach his other device.
    alice.send(conv.id, "new for both").await;
    bob.received_text("new for both").await;
    bob2.received_text("new for both").await;
    bob.send(conv.id, "from bob's first pc").await;
    alice.received_text("from bob's first pc").await;
    let copy = bob2.received_text("from bob's first pc").await;
    assert!(copy.mine, "a copy of Bob's own message is his");

    // The new device has only what was sent after it joined.
    let on_bob2 = bob2.texts(conv.id).await;
    assert_eq!(
        on_bob2,
        vec![
            (false, "new for both".to_owned()),
            (true, "from bob's first pc".to_owned())
        ]
    );
    let on_bob = bob.texts(conv.id).await;
    assert_eq!(on_bob.len(), 5, "{on_bob:?}");

    // Bob revokes the second device from the first one: nothing reaches it any more.
    bob.service.device_revoke(bob2_device).await.unwrap();
    let revoked_at = api.now().await;
    alice
        .wait(|s| matches!(s, Seen::Notice(_, DeviceNotice::DeviceRevoked { device_id, .. }) if *device_id == bob2_device))
        .await;
    alice.send(conv.id, "after the revocation").await;
    bob.received_text("after the revocation").await;
    bob.send(conv.id, "bob again").await;
    alice.received_text("bob again").await;
    assert_eq!(
        api.envelopes_to_since(bob2_device, revoked_at).await,
        0,
        "no envelope was addressed to the revoked device"
    );

    // The server only ever held ciphertext.
    for text in [
        "hello bob",
        "hi alice",
        "new for both",
        "after the revocation",
    ] {
        let hits: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM message_envelopes WHERE position(convert_to($1, 'UTF8') in ciphertext) > 0",
        )
        .bind(text)
        .fetch_one(&api.pool)
        .await
        .unwrap();
        assert_eq!(hits, 0, "plaintext {text:?} found on the server");
    }

    for l in [&alice, &bob, &bob2] {
        l.shutdown.cancel();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    api.drop_database().await;
}

// ---- A4-T09: the invite handshake (M3 demo) ---------------------------------------------------

impl Api {
    /// A published package with one release, as the catalog would have it.
    async fn package(&self, slug: &str, by: Uuid) -> Uuid {
        let package: Uuid = sqlx::query_scalar(
            "INSERT INTO packages (slug, title, status, created_by) VALUES ($1, $1, 'published', $2) RETURNING id",
        )
        .bind(slug)
        .bind(by)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO trust_bundles (version, bundle, signature) VALUES (1, '\\x00', $1) ON CONFLICT DO NOTHING")
            .bind(vec![0u8; 64])
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO publisher_keys (key_id, public_key, holder_user_id, label, not_before, not_after, trust_version)
             VALUES ('0123456789abcdef0123456789abcdef', $1, $2, 'test', now() - interval '1 day', now() + interval '1 year', 1)
             ON CONFLICT DO NOTHING",
        )
        .bind(vec![7u8; 32])
        .bind(by)
        .execute(&self.pool)
        .await
        .unwrap();
        let version: Uuid = sqlx::query_scalar(
            "INSERT INTO package_versions (package_id, platform, sequence, version_label, state, pack_count, total_size,
                file_count, chunk_count, manifest_object, manifest_size, manifest_blake3, signature, publisher_key_id,
                created_by, published_at)
             VALUES ($1, 'linux-x86_64', 1, '1.0', 'published', 1, 1234, 1, 1, 'm', 10, $2, $3,
                '0123456789abcdef0123456789abcdef', $4, now())
             RETURNING id",
        )
        .bind(package)
        .bind(vec![1u8; 32])
        .bind(vec![2u8; 64])
        .bind(by)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO package_releases (package_id, platform, version_id, updated_by) VALUES ($1, 'linux-x86_64', $2, $3)")
            .bind(package)
            .bind(version)
            .bind(by)
            .execute(&self.pool)
            .await
            .unwrap();
        package
    }
}

/// Agent 2's library and launcher, faked: a fixed install state and a record of launches.
struct FakeGames {
    check: std::sync::Mutex<GameCheck>,
    launches: mpsc::UnboundedSender<(Uuid, Option<String>)>,
}

impl Games for FakeGames {
    fn check(&self, _: Uuid, _: Uuid) -> BoxFuture<'_, GameCheck> {
        let c = *self.check.lock().unwrap();
        Box::pin(async move { c })
    }
    fn launch_join(
        &self,
        _: Uuid,
        package: Uuid,
        join_secret: Option<String>,
    ) -> BoxFuture<'_, Result<(), LaunchError>> {
        let _ = self.launches.send((package, join_secret));
        Box::pin(async { Ok(()) })
    }
}

impl Launcher {
    async fn invite_state(&mut self, id: Uuid, want: InviteState) -> Invite {
        match self
            .wait(|s| matches!(s, Seen::InviteChanged(i) if i.id == id && i.state == want))
            .await
        {
            Seen::InviteChanged(i) => i,
            _ => unreachable!(),
        }
    }

    fn install_progress(&self, package: PackageRef, done: u64, phase: InstallPhase) {
        self.bus.publish(AppEvent::InstallProgress(InstallProgress {
            package,
            phase,
            bytes_done: done,
            bytes_total: 100,
            bytes_per_second: 10,
            eta_seconds: None,
            connections: 1,
        }));
    }

    fn install_finished(&self, package: PackageRef, outcome: InstallOutcome) {
        self.bus.publish(AppEvent::InstallFinished(InstallFinished {
            package,
            outcome,
        }));
    }
}

async fn next_launch(
    rx: &mut mpsc::UnboundedReceiver<(Uuid, Option<String>)>,
) -> (Uuid, Option<String>) {
    tokio::time::timeout(Duration::from_secs(15), rx.recv())
        .await
        .expect("a launch in time")
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs PostgreSQL: set VGAMES_TEST_DATABASE_URL"]
async fn invite_install_ready_join_handshake() {
    let api = start_api().await;
    let alice_id = api.user("alice").await;
    let bob_id = api.user("bob").await;
    api.befriend(alice_id, bob_id).await;
    let game = api.package("arena", alice_id).await;
    let other = api.package("racer", alice_id).await;

    let mut alice = launcher(&api, "alice", alice_id).await;
    let mut bob = launcher(&api, "bob", bob_id).await;
    alice.device().await;
    bob.device().await;
    let (launch_tx, mut launches) = mpsc::unbounded_channel();
    let games = Arc::new(FakeGames {
        check: std::sync::Mutex::new(GameCheck::Missing),
        launches: launch_tx,
    });
    bob.service.set_games(games.clone());

    // 1. Missing game: invite with a join secret → accept → install dialog at once →
    //    progress → ready → invite.join over Olm → launch with the secret → joined.
    let sent = alice
        .service
        .invite_send(
            bob_id,
            game,
            Some("gg?".into()),
            Some(" 10.0.0.2:27015 ".into()),
        )
        .await
        .unwrap();
    assert!(sent.has_join_secret);
    assert_eq!(sent.state, InviteState::Pending);
    let got = match bob
        .wait(|s| matches!(s, Seen::InviteReceived(i) if i.id == sent.id))
        .await
    {
        Seen::InviteReceived(i) => i,
        _ => unreachable!(),
    };
    assert!(
        !got.has_join_secret,
        "the secret never reaches the invitee's UI model"
    );
    assert_eq!(got.package.id, game);
    bob.service.invite_accept(sent.id).await.unwrap();
    let package = match bob
        .wait(|s| matches!(s, Seen::InstallRequested(id, _, _) if *id == sent.id))
        .await
    {
        Seen::InstallRequested(_, p, reason) => {
            assert_eq!(reason, InviteInstallReason::Missing);
            p
        }
        _ => unreachable!(),
    };
    assert_eq!(package.package_id, game);
    alice.invite_state(sent.id, InviteState::Accepted).await;
    // The normal install path runs (signature check first); progress is reported.
    bob.install_progress(package, 0, InstallPhase::VerifyingManifest);
    bob.install_progress(package, 30, InstallPhase::Downloading);
    let installing = alice.invite_state(sent.id, InviteState::Installing).await;
    assert!(
        installing
            .progress
            .is_some_and(|p| (0.0..=1.0).contains(&p))
    );
    // Not ready before the install finished.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let now = alice.service.invites_list().await.unwrap();
    assert_eq!(
        now.iter().find(|i| i.id == sent.id).unwrap().state,
        InviteState::Installing
    );
    assert!(launches.try_recv().is_err());

    *games.check.lock().unwrap() = GameCheck::Current;
    bob.install_finished(package, InstallOutcome::Installed);
    alice.invite_state(sent.id, InviteState::Ready).await;
    let (launched, secret) = next_launch(&mut launches).await;
    assert_eq!(launched, game);
    assert_eq!(secret.as_deref(), Some("10.0.0.2:27015"));
    alice.invite_state(sent.id, InviteState::Joined).await;
    // The server never held the secret.
    let hits: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM game_invites WHERE message LIKE '%10.0.0.2%')
              + (SELECT count(*) FROM message_envelopes WHERE position(convert_to('10.0.0.2', 'UTF8') in ciphertext) > 0)",
    )
    .fetch_one(&api.pool)
    .await
    .unwrap();
    assert_eq!(hits, 0);

    // 2. Installed and current, no secret: ready at once, normal launch without arguments.
    let sent = alice
        .service
        .invite_send(bob_id, other, None, None)
        .await
        .unwrap();
    assert!(!sent.has_join_secret);
    bob.wait(|s| matches!(s, Seen::InviteReceived(i) if i.id == sent.id))
        .await;
    bob.service.invite_accept(sent.id).await.unwrap();
    alice.invite_state(sent.id, InviteState::Ready).await;
    let (launched, secret) = next_launch(&mut launches).await;
    assert_eq!((launched, secret), (other, None));
    alice.invite_state(sent.id, InviteState::Joined).await;

    // 3. The install fails its signature check: failed, never ready, nothing launched.
    *games.check.lock().unwrap() = GameCheck::Missing;
    let sent = alice
        .service
        .invite_send(bob_id, game, None, Some("lobby-7".into()))
        .await
        .unwrap();
    bob.wait(|s| matches!(s, Seen::InviteReceived(i) if i.id == sent.id))
        .await;
    bob.service.invite_accept(sent.id).await.unwrap();
    bob.wait(|s| matches!(s, Seen::InstallRequested(id, _, _) if *id == sent.id))
        .await;
    bob.install_finished(
        package,
        InstallOutcome::Failed {
            code: "signature_invalid".into(),
            message: "The package signature does not verify".into(),
        },
    );
    let failed = alice.invite_state(sent.id, InviteState::Failed).await;
    assert_eq!(failed.failure_reason, Some(InviteFailure::InstallFailed));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(launches.try_recv().is_err());

    // 4. A pending invite cancelled by the sender reaches the invitee as cancelled.
    let sent = alice
        .service
        .invite_send(bob_id, other, None, None)
        .await
        .unwrap();
    bob.wait(|s| matches!(s, Seen::InviteReceived(i) if i.id == sent.id))
        .await;
    alice.service.invite_cancel(sent.id).await.unwrap();
    bob.invite_state(sent.id, InviteState::Cancelled).await;
    assert!(matches!(
        bob.service.invite_accept(sent.id).await,
        Err(vgames_desktop_lib::social::model::SocialError::Conflict { .. })
    ));
    // An invalid secret is refused before anything is sent.
    assert!(matches!(
        alice
            .service
            .invite_send(bob_id, other, None, Some("a b".into()))
            .await,
        Err(vgames_desktop_lib::social::model::SocialError::InvalidInput { .. })
    ));

    for l in [&alice, &bob] {
        l.shutdown.cancel();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    api.drop_database().await;
}
