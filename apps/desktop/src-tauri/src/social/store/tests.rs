//! A4-T02 acceptance through the store: two and three in-process launchers, each with its own
//! SQLite database and keys, talking through a fake relay with the server's semantics
//! (device directory, atomic one-time-key claims with fallback, per-device inboxes).

use std::collections::HashMap;

use rusqlite::Connection;
use time::OffsetDateTime;
use uuid::Uuid;
use vgames_proto::social::{ClaimedKey, DeviceKeys, InboxEnvelope, SignedOneTimeKey};

use super::*;
use crate::social::payload::Payload;

const SERVER: Uuid = Uuid::from_u128(0x01920000_0000_7000_8000_00000000abcd);
const NOW: i64 = 1_790_000_000;

fn memory_db() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    crate::db::migrations::run(&mut conn).unwrap();
    add_server(&conn);
    conn
}

fn add_server(conn: &Connection) {
    conn.execute(
        "INSERT OR IGNORE INTO servers (id, url, name, root_public_key, root_fingerprint, added_at, is_active)
         VALUES (?1, 'https://vgames.test', 'test', zeroblob(32), 'VG1-TEST', 0, 1)",
        [SERVER.to_string()],
    )
    .unwrap();
}

fn test_keys() -> Keys {
    Keys::new(
        SecretKey32::generate().unwrap(),
        &SecretKey32::generate().unwrap(),
    )
    .unwrap()
}

/// The server side, in memory.
#[derive(Default)]
struct Relay {
    devices: HashMap<Uuid, (Uuid, DeviceKeys)>,
    otks: HashMap<Uuid, Vec<SignedOneTimeKey>>,
    fallback: HashMap<Uuid, SignedOneTimeKey>,
    inbox: HashMap<Uuid, Vec<InboxEnvelope>>,
}

impl Relay {
    fn register(&mut self, l: &mut Launcher) {
        let device = Uuid::now_v7();
        let signed = signed_device_keys(&l.db, &l.keys, SERVER).unwrap();
        set_device_id(&l.db, SERVER, device, NOW).unwrap();
        l.device = device;
        self.devices.insert(
            device,
            (
                l.user,
                DeviceKeys {
                    device_id: device,
                    display_name: Some(l.name.to_owned()),
                    identity_key: signed.identity_key,
                    signing_key: signed.signing_key,
                    keys_signature: signed.keys_signature,
                    created_at: OffsetDateTime::UNIX_EPOCH,
                },
            ),
        );
        self.upload(l, crypto::INITIAL_ONE_TIME_KEYS, true);
    }

    fn upload(&mut self, l: &mut Launcher, count: usize, fallback: bool) {
        let up = prepare_key_upload(&mut l.db, &l.keys, SERVER, count, fallback, NOW).unwrap();
        self.otks
            .entry(l.device)
            .or_default()
            .extend(up.one_time_keys);
        if let Some(f) = up.fallback_key {
            self.fallback.insert(l.device, f);
        }
        mark_keys_published(&mut l.db, &l.keys, SERVER, NOW).unwrap();
    }

    fn devices_of(&self, user: Uuid) -> Vec<DeviceKeys> {
        let mut v: Vec<DeviceKeys> = self
            .devices
            .values()
            .filter(|(u, _)| *u == user)
            .map(|(_, d)| d.clone())
            .collect();
        v.sort_by_key(|d| d.device_id);
        v
    }

    /// Atomic claim: one unclaimed key per device, else the fallback key.
    fn claim(&mut self, devices: &[Uuid]) -> Vec<ClaimedKey> {
        devices
            .iter()
            .filter_map(|d| {
                let list = self.otks.entry(*d).or_default();
                if list.is_empty() {
                    self.fallback.get(d).map(|k| claimed(*d, k, true))
                } else {
                    Some(claimed(*d, &list.remove(0), false))
                }
            })
            .collect()
    }

    fn deliver(&mut self, from: &Launcher, conversation: Uuid, envelopes: &[OutgoingEnvelope]) {
        let sender_identity = self.devices[&from.device].1.identity_key.clone();
        for e in envelopes {
            self.inbox
                .entry(e.recipient_device_id)
                .or_default()
                .push(InboxEnvelope {
                    id: Uuid::now_v7(),
                    conversation_id: conversation,
                    sender_user_id: from.user,
                    sender_device_id: from.device,
                    sender_identity_key: sender_identity.clone(),
                    algorithm: "olm.v1".into(),
                    olm_message_type: e.olm_message_type,
                    ciphertext: e.ciphertext.clone(),
                    created_at: OffsetDateTime::UNIX_EPOCH,
                });
        }
    }

    fn take_inbox(&mut self, l: &Launcher) -> Vec<InboxEnvelope> {
        std::mem::take(self.inbox.entry(l.device).or_default())
    }
}

fn claimed(device: Uuid, k: &SignedOneTimeKey, is_fallback: bool) -> ClaimedKey {
    ClaimedKey {
        device_id: device,
        key_id: k.key_id.clone(),
        public_key: k.public_key.clone(),
        signature: k.signature.clone(),
        is_fallback,
    }
}

struct Launcher {
    name: &'static str,
    user: Uuid,
    device: Uuid,
    db: Connection,
    keys: Keys,
}

impl Launcher {
    fn new(name: &'static str, user: Uuid) -> Self {
        let mut db = memory_db();
        let keys = test_keys();
        ensure_account(&mut db, &keys, SERVER, user, NOW).unwrap();
        Self {
            name,
            user,
            device: Uuid::nil(),
            db,
            keys,
        }
    }

    /// A second device of the same user.
    fn second_device(name: &'static str, of: &Launcher) -> Self {
        Self::new(name, of.user)
    }

    /// Fetches and pins every device of `users` (what the messaging service does).
    fn learn(&mut self, relay: &Relay, users: &[Uuid]) -> Vec<DeviceChanges> {
        users
            .iter()
            .map(|u| {
                sync_user_devices(
                    &mut self.db,
                    &self.keys,
                    SERVER,
                    *u,
                    &relay.devices_of(*u),
                    NOW,
                )
                .unwrap()
            })
            .collect()
    }

    /// Full send path: fan-out to every device of `users` (and own other devices).
    fn send(
        &mut self,
        relay: &mut Relay,
        conversation: Uuid,
        users: &[Uuid],
        body: &str,
    ) -> (Uuid, Vec<OutgoingEnvelope>) {
        let mut all = users.to_vec();
        all.push(self.user);
        self.learn(relay, &all);
        let devices: Vec<Uuid> = all
            .iter()
            .flat_map(|u| relay.devices_of(*u))
            .map(|d| d.device_id)
            .filter(|d| *d != self.device)
            .collect();
        let missing = devices_without_session(&self.db, SERVER, &devices).unwrap();
        let claimed = relay.claim(&missing);
        let cmid = Uuid::now_v7();
        let payload = Payload::text(
            conversation,
            cmid,
            OffsetDateTime::from_unix_timestamp(NOW).unwrap(),
            body,
        )
        .unwrap();
        // Stored first (pending), then taken from the outbox, like the messaging service does.
        create_outgoing(
            &mut self.db,
            &self.keys,
            SERVER,
            &payload,
            MessageBody::Text {
                text: body.to_owned(),
            },
            NOW,
        )
        .unwrap();
        let item = outbox_due(&self.db, &self.keys, SERVER, NOW, 100)
            .unwrap()
            .into_iter()
            .find(|i| i.client_message_id == cmid)
            .unwrap();
        let out = encrypt_for(
            &mut self.db,
            &self.keys,
            SERVER,
            &devices,
            &claimed,
            &item.plaintext,
            NOW,
        )
        .unwrap();
        assert!(
            out.missing.is_empty() && out.blocked.is_empty() && out.unknown.is_empty(),
            "{out:?}"
        );
        relay.deliver(self, conversation, &out.envelopes);
        assert!(outbox_sent(&mut self.db, SERVER, cmid).unwrap().is_some());
        (cmid, out.envelopes)
    }

    fn receive_all(&mut self, relay: &mut Relay) -> Vec<Received> {
        relay
            .take_inbox(self)
            .iter()
            .map(|e| self.receive_one(e))
            .collect()
    }

    fn receive_one(&mut self, e: &InboxEnvelope) -> Received {
        receive(&mut self.db, &self.keys, SERVER, self.user, e, NOW).unwrap()
    }
}

fn text_of(r: &Received) -> String {
    match r {
        Received::Message(Message {
            body: MessageBody::Text { text },
            ..
        }) => text.clone(),
        other => panic!("not a text message: {other:?}"),
    }
}

fn users() -> (Uuid, Uuid, Uuid) {
    (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7())
}

#[test]
fn pre_key_then_normal_messages_through_the_store() {
    let (ua, ub, _) = users();
    let mut relay = Relay::default();
    let mut alice = Launcher::new("alice", ua);
    let mut bob = Launcher::new("bob", ub);
    relay.register(&mut alice);
    relay.register(&mut bob);
    let conv = Uuid::now_v7();

    let (_, env) = alice.send(&mut relay, conv, &[ub], "hi bob");
    assert_eq!(env.len(), 1);
    assert_eq!(env[0].olm_message_type, crypto::MESSAGE_TYPE_PRE_KEY);
    assert_eq!(
        relay.otks[&bob.device].len(),
        crypto::INITIAL_ONE_TIME_KEYS - 1,
        "one key claimed"
    );

    bob.learn(&relay, &[ua]);
    let got = bob.receive_all(&mut relay);
    assert_eq!(text_of(&got[0]), "hi bob");

    let (_, env) = bob.send(&mut relay, conv, &[ua], "hi alice");
    assert_eq!(
        env[0].olm_message_type,
        crypto::MESSAGE_TYPE_NORMAL,
        "Bob answers on the inbound session"
    );
    assert_eq!(text_of(&alice.receive_all(&mut relay)[0]), "hi alice");

    let (_, env) = alice.send(&mut relay, conv, &[ub], "normal now");
    assert_eq!(env[0].olm_message_type, crypto::MESSAGE_TYPE_NORMAL);
    assert_eq!(text_of(&bob.receive_all(&mut relay)[0]), "normal now");

    // History on both sides, in order, with direction.
    let a = list_messages(&alice.db, &alice.keys, SERVER, conv, None, 50).unwrap();
    let texts: Vec<(bool, String)> = a
        .iter()
        .map(|m| match &m.body {
            MessageBody::Text { text } => (m.mine, text.clone()),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        texts,
        vec![
            (true, "hi bob".to_string()),
            (false, "hi alice".to_string()),
            (true, "normal now".to_string())
        ]
    );
    assert!(
        a.iter()
            .filter(|m| m.mine)
            .all(|m| m.status == MessageStatus::Sent)
    );
    assert!(
        a.iter()
            .filter(|m| !m.mine)
            .all(|m| m.status == MessageStatus::Received)
    );
    // Pagination: before the last message.
    let older = list_messages(&alice.db, &alice.keys, SERVER, conv, Some(a[2].id), 50).unwrap();
    assert_eq!(older.len(), 2);
}

#[test]
fn duplicates_are_ignored() {
    let (ua, ub, _) = users();
    let mut relay = Relay::default();
    let mut alice = Launcher::new("alice", ua);
    let mut bob = Launcher::new("bob", ub);
    relay.register(&mut alice);
    relay.register(&mut bob);
    bob.learn(&relay, &[ua]);
    let conv = Uuid::now_v7();

    alice.send(&mut relay, conv, &[ub], "once");
    let inbox = relay.take_inbox(&bob);
    assert_eq!(text_of(&bob.receive_one(&inbox[0])), "once");
    // Redelivery of the same envelope (the ack was lost).
    assert_eq!(bob.receive_one(&inbox[0]), Received::Duplicate);

    // The sender retried the same client_message_id with a fresh encryption.
    let (cmid, _) = alice.send(&mut relay, conv, &[ub], "twice");
    let first = relay.take_inbox(&bob);
    assert_eq!(text_of(&bob.receive_one(&first[0])), "twice");
    let payload = Payload::text(
        conv,
        cmid,
        OffsetDateTime::from_unix_timestamp(NOW).unwrap(),
        "twice",
    )
    .unwrap();
    let again = encrypt_for(
        &mut alice.db,
        &alice.keys,
        SERVER,
        &[bob.device],
        &[],
        &payload.encode().unwrap(),
        NOW,
    )
    .unwrap();
    relay.deliver(&alice, conv, &again.envelopes);
    let resent = relay.take_inbox(&bob);
    assert_eq!(bob.receive_one(&resent[0]), Received::Duplicate);

    let history = list_messages(&bob.db, &bob.keys, SERVER, conv, None, 50).unwrap();
    assert_eq!(history.len(), 2, "each message stored once");
}

#[test]
fn out_of_order_delivery() {
    let (ua, ub, _) = users();
    let mut relay = Relay::default();
    let mut alice = Launcher::new("alice", ua);
    let mut bob = Launcher::new("bob", ub);
    relay.register(&mut alice);
    relay.register(&mut bob);
    bob.learn(&relay, &[ua]);
    let conv = Uuid::now_v7();

    // Establish, then send five messages that arrive shuffled.
    alice.send(&mut relay, conv, &[ub], "m0");
    bob.receive_all(&mut relay);
    bob.send(&mut relay, conv, &[ua], "ack");
    alice.receive_all(&mut relay);
    for i in 1..=5 {
        alice.send(&mut relay, conv, &[ub], &format!("m{i}"));
    }
    let inbox = relay.take_inbox(&bob);
    let mut got = Vec::new();
    for i in [3usize, 1, 5, 2, 4] {
        got.push(text_of(&bob.receive_one(&inbox[i - 1])));
    }
    assert_eq!(got, ["m3", "m1", "m5", "m2", "m4"]);

    // A pre-key burst (before any answer) also arrives out of order.
    let (uc, _, _) = users();
    let mut carol = Launcher::new("carol", uc);
    relay.register(&mut carol);
    bob.learn(&relay, &[uc]);
    for i in 1..=3 {
        carol.send(&mut relay, conv, &[ub], &format!("c{i}"));
    }
    let inbox = relay.take_inbox(&bob);
    assert!(
        inbox
            .iter()
            .all(|e| e.olm_message_type == crypto::MESSAGE_TYPE_PRE_KEY)
    );
    let texts: Vec<String> = [2usize, 0, 1]
        .iter()
        .map(|i| text_of(&bob.receive_one(&inbox[*i])))
        .collect();
    assert_eq!(texts, ["c3", "c1", "c2"]);
}

#[test]
fn fallback_key_after_one_time_keys_run_out() {
    let (ua, ub, uc) = users();
    let mut relay = Relay::default();
    let mut alice = Launcher::new("alice", ua);
    let mut bob = Launcher::new("bob", ub);
    let mut carol = Launcher::new("carol", uc);
    relay.register(&mut alice);
    relay.register(&mut bob);
    relay.register(&mut carol);
    // Everything but one of Bob's keys was claimed by others.
    let n = relay.otks[&bob.device].len();
    relay.otks.get_mut(&bob.device).unwrap().truncate(1);
    assert!(n > 1);
    bob.learn(&relay, &[ua, uc]);
    let conv = Uuid::now_v7();

    alice.send(&mut relay, conv, &[ub], "took the last one-time key");
    assert!(relay.otks[&bob.device].is_empty());
    carol.send(&mut relay, conv, &[ub], "got the fallback key");
    let got = bob.receive_all(&mut relay);
    assert_eq!(text_of(&got[0]), "took the last one-time key");
    assert_eq!(text_of(&got[1]), "got the fallback key");

    // Top-up: new keys are generated, only the new ones are published.
    relay.upload(&mut bob, 10, false);
    assert_eq!(relay.otks[&bob.device].len(), 10);
}

#[test]
fn state_survives_a_restart_and_needs_the_keychain_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("social.sqlite3");
    let (ua, ub, _) = users();
    let pickle_key = SecretKey32::generate().unwrap();
    let chat_key = SecretKey32::generate().unwrap();
    let open = || {
        let mut conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        crate::db::migrations::run(&mut conn).unwrap();
        add_server(&conn);
        conn
    };
    let mut relay = Relay::default();
    let mut alice = Launcher::new("alice", ua);
    let mut bob = Launcher {
        name: "bob",
        user: ub,
        device: Uuid::nil(),
        db: open(),
        keys: Keys::new(pickle_key.clone(), &chat_key).unwrap(),
    };
    ensure_account(&mut bob.db, &bob.keys, SERVER, ub, NOW).unwrap();
    relay.register(&mut alice);
    relay.register(&mut bob);
    bob.learn(&relay, &[ua]);
    let conv = Uuid::now_v7();
    alice.send(&mut relay, conv, &[ub], "before restart");
    assert_eq!(text_of(&bob.receive_all(&mut relay)[0]), "before restart");
    let identity = account_info(&bob.db, &bob.keys, SERVER)
        .unwrap()
        .unwrap()
        .identity_key;
    let device = bob.device;
    drop(bob);

    // Wrong keychain key: nothing decrypts.
    let wrong = Keys::new(
        SecretKey32::generate().unwrap(),
        &SecretKey32::generate().unwrap(),
    )
    .unwrap();
    let conn = open();
    assert!(matches!(
        account_info(&conn, &wrong, SERVER),
        Err(StoreError::Crypto(CryptoError::Pickle))
    ));
    assert!(list_messages(&conn, &wrong, SERVER, conv, None, 10).is_err());
    drop(conn);

    let mut bob = Launcher {
        name: "bob",
        user: ub,
        device,
        db: open(),
        keys: Keys::new(pickle_key, &chat_key).unwrap(),
    };
    let info = ensure_account(&mut bob.db, &bob.keys, SERVER, ub, NOW).unwrap();
    assert_eq!(info.identity_key, identity);
    assert_eq!(info.device_id, Some(device));
    assert_eq!(
        list_messages(&bob.db, &bob.keys, SERVER, conv, None, 10)
            .unwrap()
            .len(),
        1
    );
    alice.send(&mut relay, conv, &[ub], "after restart");
    assert_eq!(text_of(&bob.receive_all(&mut relay)[0]), "after restart");
    bob.send(&mut relay, conv, &[ua], "still here");
    assert_eq!(text_of(&alice.receive_all(&mut relay)[0]), "still here");
}

#[test]
fn tampered_ciphertext_is_rejected_and_harmless() {
    let (ua, ub, _) = users();
    let mut relay = Relay::default();
    let mut alice = Launcher::new("alice", ua);
    let mut bob = Launcher::new("bob", ub);
    relay.register(&mut alice);
    relay.register(&mut bob);
    bob.learn(&relay, &[ua]);
    let conv = Uuid::now_v7();

    alice.send(&mut relay, conv, &[ub], "first");
    let mut inbox = relay.take_inbox(&bob);
    let mut bytes = crypto::wire_b64_decode(&inbox[0].ciphertext).unwrap();
    let i = bytes.len() - 10;
    bytes[i] ^= 0x80;
    let mut forged = inbox[0].clone();
    forged.ciphertext = crypto::wire_b64(&bytes);
    forged.id = Uuid::now_v7();
    assert!(matches!(
        receive(&mut bob.db, &bob.keys, SERVER, ub, &forged, NOW),
        Err(ReceiveError::Undecryptable(_))
    ));
    // A server-supplied identity key that differs from the pin is refused before decrypting.
    let mut wrong_key = inbox[0].clone();
    wrong_key.sender_identity_key = OlmAccount::new().identity_key();
    assert!(matches!(
        receive(&mut bob.db, &bob.keys, SERVER, ub, &wrong_key, NOW),
        Err(ReceiveError::KeyMismatch { .. })
    ));
    // Nothing was stored and the real message still decrypts.
    assert!(
        list_messages(&bob.db, &bob.keys, SERVER, conv, None, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(text_of(&bob.receive_one(&inbox.remove(0))), "first");
}

#[test]
fn unknown_sender_is_reported_until_its_device_is_pinned() {
    let (ua, ub, _) = users();
    let mut relay = Relay::default();
    let mut alice = Launcher::new("alice", ua);
    let mut bob = Launcher::new("bob", ub);
    relay.register(&mut alice);
    relay.register(&mut bob);
    let conv = Uuid::now_v7();
    alice.send(&mut relay, conv, &[ub], "who am i");
    let inbox = relay.take_inbox(&bob);
    assert!(matches!(
        receive(&mut bob.db, &bob.keys, SERVER, ub, &inbox[0], NOW),
        Err(ReceiveError::UnknownSender { .. })
    ));
    bob.learn(&relay, &[ua]);
    assert_eq!(text_of(&bob.receive_one(&inbox[0])), "who am i");
}

#[test]
fn party_fan_out_reaches_members_and_own_devices() {
    let (ua, ub, uc) = users();
    let mut relay = Relay::default();
    let mut alice = Launcher::new("alice", ua);
    let mut alice_laptop = Launcher::second_device("alice-laptop", &alice);
    let mut bob = Launcher::new("bob", ub);
    let mut carol = Launcher::new("carol", uc);
    for l in [&mut alice, &mut alice_laptop, &mut bob, &mut carol] {
        relay.register(l);
    }
    for l in [&mut alice_laptop, &mut bob, &mut carol] {
        l.learn(&relay, &[ua, ub, uc]);
    }
    let party = Uuid::now_v7();
    let (_, env) = alice.send(&mut relay, party, &[ub, uc], "party time");
    assert_eq!(env.len(), 3, "bob, carol and alice's laptop");

    for l in [&mut bob, &mut carol] {
        let got = l.receive_all(&mut relay);
        assert_eq!(text_of(&got[0]), "party time");
        assert!(matches!(&got[0], Received::Message(m) if !m.mine));
    }
    let copy = alice_laptop.receive_all(&mut relay);
    assert!(matches!(&copy[0], Received::Message(m) if m.mine && m.status == MessageStatus::Sent));
}

#[test]
fn safety_numbers_match_and_device_changes_need_reverification() {
    let (ua, ub, _) = users();
    let mut relay = Relay::default();
    let mut alice = Launcher::new("alice", ua);
    let mut bob = Launcher::new("bob", ub);
    relay.register(&mut alice);
    relay.register(&mut bob);
    let first = alice.learn(&relay, &[ub]);
    assert!(
        first[0].first_sight && first[0].notices(ub).is_empty(),
        "first sight is not a notice"
    );
    bob.learn(&relay, &[ua]);

    let a_view = contact_security(&alice.db, &alice.keys, SERVER, ub).unwrap();
    let b_view = contact_security(&bob.db, &bob.keys, SERVER, ua).unwrap();
    assert_eq!(a_view.safety_number, b_view.safety_number);
    assert_eq!(a_view.safety_number.len(), 60);
    assert!(!a_view.verified && !a_view.needs_reverification);

    let verified = set_verified(&alice.db, &alice.keys, SERVER, ub, true, NOW).unwrap();
    assert!(verified.verified);

    // Bob signs in on a laptop: Alice sees a notice and must verify again.
    let mut laptop = Launcher::second_device("bob-laptop", &bob);
    relay.register(&mut laptop);
    let changes = sync_user_devices(
        &mut alice.db,
        &alice.keys,
        SERVER,
        ub,
        &relay.devices_of(ub),
        NOW + 10,
    )
    .unwrap();
    assert_eq!(changes.new.len(), 1);
    assert!(matches!(
        changes.notices(ub)[0],
        DeviceNotice::NewDevice { .. }
    ));
    let after = contact_security(&alice.db, &alice.keys, SERVER, ub).unwrap();
    assert!(!after.verified && after.needs_reverification);
    assert_ne!(after.safety_number, a_view.safety_number);
    assert!(
        after
            .devices
            .iter()
            .any(|d| d.state == ContactDeviceState::New)
    );
    // Both sides agree again once Bob knows his own laptop.
    bob.learn(&relay, &[ub]);
    let b_after = contact_security(&bob.db, &bob.keys, SERVER, ua).unwrap();
    assert_eq!(b_after.safety_number, after.safety_number);
    assert!(
        set_verified(&alice.db, &alice.keys, SERVER, ub, true, NOW + 20)
            .unwrap()
            .verified
    );
}

#[test]
fn a_changed_key_blocks_sending_until_trusted() {
    let (ua, ub, _) = users();
    let mut relay = Relay::default();
    let mut alice = Launcher::new("alice", ua);
    let mut bob = Launcher::new("bob", ub);
    relay.register(&mut alice);
    relay.register(&mut bob);
    bob.learn(&relay, &[ua]);
    let conv = Uuid::now_v7();
    alice.send(&mut relay, conv, &[ub], "hello");
    bob.receive_all(&mut relay);

    // The server now presents a different key for Bob's device id (reinstall, or an attack).
    let mut new_bob = Launcher::new("bob-reinstalled", ub);
    let signed = signed_device_keys(&new_bob.db, &new_bob.keys, SERVER).unwrap();
    set_device_id(&new_bob.db, SERVER, bob.device, NOW).unwrap();
    new_bob.device = bob.device;
    {
        let entry = relay.devices.get_mut(&bob.device).unwrap();
        entry.1.identity_key = signed.identity_key;
        entry.1.signing_key = signed.signing_key;
        entry.1.keys_signature = signed.keys_signature;
    }
    relay.otks.remove(&bob.device);
    relay.upload(&mut new_bob, 5, true);

    let changes = sync_user_devices(
        &mut alice.db,
        &alice.keys,
        SERVER,
        ub,
        &relay.devices_of(ub),
        NOW,
    )
    .unwrap();
    assert_eq!(changes.key_changed, vec![bob.device]);
    // Seeing the same changed key again is not a second notice.
    let again = sync_user_devices(
        &mut alice.db,
        &alice.keys,
        SERVER,
        ub,
        &relay.devices_of(ub),
        NOW,
    )
    .unwrap();
    assert!(again.is_empty());

    let payload =
        Payload::text(conv, Uuid::now_v7(), OffsetDateTime::UNIX_EPOCH, "blocked?").unwrap();
    let out = encrypt_for(
        &mut alice.db,
        &alice.keys,
        SERVER,
        &[bob.device],
        &[],
        &payload.encode().unwrap(),
        NOW,
    )
    .unwrap();
    assert!(out.envelopes.is_empty());
    assert_eq!(out.blocked, vec![(ub, bob.device)]);
    assert_eq!(
        contact_security(&alice.db, &alice.keys, SERVER, ub)
            .unwrap()
            .devices[0]
            .state,
        ContactDeviceState::KeyChanged
    );

    // After comparing safety numbers, Alice trusts the new key; a new session is claimed.
    assert!(trust_device(&mut alice.db, SERVER, ub, bob.device, NOW).unwrap());
    new_bob.learn(&relay, &[ua]);
    alice.send(&mut relay, conv, &[ub], "hi again");
    assert_eq!(text_of(&new_bob.receive_all(&mut relay)[0]), "hi again");
}

#[test]
fn revoked_devices_stop_receiving() {
    let (ua, ub, _) = users();
    let mut relay = Relay::default();
    let mut alice = Launcher::new("alice", ua);
    let mut bob = Launcher::new("bob", ub);
    let mut bob_laptop = Launcher::second_device("bob-laptop", &bob);
    relay.register(&mut alice);
    relay.register(&mut bob);
    relay.register(&mut bob_laptop);
    let conv = Uuid::now_v7();
    let (_, env) = alice.send(&mut relay, conv, &[ub], "to both");
    assert_eq!(env.len(), 2);

    let laptop_entry = relay.devices.remove(&bob_laptop.device).unwrap();
    let changes = alice.learn(&relay, &[ub]);
    assert_eq!(changes[0].revoked, vec![bob_laptop.device]);
    assert!(matches!(
        changes[0].notices(ub)[0],
        DeviceNotice::DeviceRevoked { .. }
    ));
    let (_, env) = alice.send(&mut relay, conv, &[ub], "phone only");
    assert_eq!(env.len(), 1);
    assert_eq!(env[0].recipient_device_id, bob.device);

    // A revoked device never comes back, even if a server lists it again.
    relay.devices.insert(bob_laptop.device, laptop_entry);
    assert!(alice.learn(&relay, &[ub])[0].is_empty());
    let payload = Payload::text(conv, Uuid::now_v7(), OffsetDateTime::UNIX_EPOCH, "x").unwrap();
    let out = encrypt_for(
        &mut alice.db,
        &alice.keys,
        SERVER,
        &[bob.device, bob_laptop.device],
        &[],
        &payload.encode().unwrap(),
        NOW,
    )
    .unwrap();
    assert_eq!(out.envelopes.len(), 1);
    assert_eq!(out.envelopes[0].recipient_device_id, bob.device);
    assert_eq!(
        mark_device_revoked(&alice.db, SERVER, bob_laptop.device, NOW).unwrap(),
        Some(ub)
    );
    assert_eq!(
        mark_device_revoked(&alice.db, SERVER, Uuid::now_v7(), NOW).unwrap(),
        None
    );
}

#[test]
fn bodies_and_pickles_are_encrypted_at_rest() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rest.sqlite3");
    let (ua, ub, _) = users();
    let mut relay = Relay::default();
    let mut alice = Launcher::new("alice", ua);
    let mut conn = Connection::open(&path).unwrap();
    crate::db::migrations::run(&mut conn).unwrap();
    add_server(&conn);
    let mut bob = Launcher {
        name: "bob",
        user: ub,
        device: Uuid::nil(),
        db: conn,
        keys: test_keys(),
    };
    ensure_account(&mut bob.db, &bob.keys, SERVER, ub, NOW).unwrap();
    relay.register(&mut alice);
    relay.register(&mut bob);
    bob.learn(&relay, &[ua]);
    let conv = Uuid::now_v7();
    let secret = "the cake is a lie 5e3f9a";
    alice.send(&mut relay, conv, &[ub], secret);
    bob.receive_all(&mut relay);
    let payload = Payload::text(
        conv,
        Uuid::now_v7(),
        OffsetDateTime::UNIX_EPOCH,
        "queued secret 77ab1c",
    )
    .unwrap();
    create_outgoing(
        &mut bob.db,
        &bob.keys,
        SERVER,
        &payload,
        MessageBody::Text {
            text: "queued secret 77ab1c".into(),
        },
        NOW,
    )
    .unwrap();
    bob.db
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    drop(bob);

    let raw = std::fs::read(&path).unwrap();
    let hay = String::from_utf8_lossy(&raw);
    assert!(!hay.contains(secret), "message body found in plaintext");
    assert!(
        !hay.contains("queued secret"),
        "outbox payload found in plaintext"
    );
    assert!(!hay.contains("\"body\""), "payload JSON found in plaintext");
}

#[test]
fn outbox_retries_with_backoff_then_fails_and_can_be_retried() {
    let (ua, _, _) = users();
    let mut relay = Relay::default();
    let mut alice = Launcher::new("alice", ua);
    relay.register(&mut alice);
    let conv = Uuid::now_v7();
    let cmid = Uuid::now_v7();
    let payload = Payload::text(
        conv,
        cmid,
        OffsetDateTime::from_unix_timestamp(NOW).unwrap(),
        "later",
    )
    .unwrap();
    let msg = create_outgoing(
        &mut alice.db,
        &alice.keys,
        SERVER,
        &payload,
        MessageBody::Text {
            text: "later".into(),
        },
        NOW,
    )
    .unwrap();
    assert_eq!(msg.status, MessageStatus::Pending);

    let due = outbox_due(&alice.db, &alice.keys, SERVER, NOW, 10).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(
        payload::decode(&due[0].plaintext).unwrap(),
        payload::Decoded::Known(payload.clone())
    );
    assert_eq!(outbox_next_due(&alice.db, SERVER).unwrap(), Some(NOW));

    let RetryOutcome::RetryAt(at) =
        outbox_attempt_failed(&mut alice.db, SERVER, cmid, NOW, "offline", false).unwrap()
    else {
        panic!("expected a retry");
    };
    assert_eq!(at, NOW + 4);
    assert!(
        outbox_due(&alice.db, &alice.keys, SERVER, NOW + 3, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        outbox_due(&alice.db, &alice.keys, SERVER, NOW + 4, 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(outbox_backoff(20), OUTBOX_MAX_BACKOFF);

    let failed =
        outbox_attempt_failed(&mut alice.db, SERVER, cmid, NOW, "forbidden", true).unwrap();
    assert!(matches!(failed, RetryOutcome::Failed { message_id, .. } if message_id == msg.id));
    assert!(
        outbox_due(&alice.db, &alice.keys, SERVER, i64::MAX - 1, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(outbox_next_due(&alice.db, SERVER).unwrap(), None);
    assert_eq!(
        get_message(&alice.db, &alice.keys, SERVER, msg.id)
            .unwrap()
            .unwrap()
            .status,
        MessageStatus::Failed
    );

    let retried = message_retry(&mut alice.db, &alice.keys, SERVER, msg.id, NOW + 100)
        .unwrap()
        .unwrap();
    assert_eq!(retried.status, MessageStatus::Pending);
    assert_eq!(
        outbox_sent(&mut alice.db, SERVER, cmid).unwrap(),
        Some((msg.id, conv))
    );
    assert_eq!(
        get_message(&alice.db, &alice.keys, SERVER, msg.id)
            .unwrap()
            .unwrap()
            .status,
        MessageStatus::Sent
    );
    assert!(
        outbox_due(&alice.db, &alice.keys, SERVER, i64::MAX - 1, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn signing_in_as_another_user_discards_the_previous_state() {
    let (ua, ub, _) = users();
    let mut relay = Relay::default();
    let mut l = Launcher::new("shared-pc", ua);
    relay.register(&mut l);
    block_add(&l.db, SERVER, ub, Some("bob"), NOW).unwrap();
    let before = account_info(&l.db, &l.keys, SERVER).unwrap().unwrap();
    let after = ensure_account(&mut l.db, &l.keys, SERVER, ub, NOW).unwrap();
    assert_eq!(after.user_id, ub);
    assert_eq!(after.device_id, None);
    assert_ne!(after.identity_key, before.identity_key);
    assert!(blocks_list(&l.db, SERVER).unwrap().is_empty());
}

#[test]
fn a_device_with_a_bad_self_signature_is_never_pinned() {
    let (ua, ub, _) = users();
    let mut relay = Relay::default();
    let mut alice = Launcher::new("alice", ua);
    let mut bob = Launcher::new("bob", ub);
    relay.register(&mut alice);
    relay.register(&mut bob);
    let mut forged = relay.devices_of(ub);
    forged[0].identity_key = OlmAccount::new().identity_key();
    let changes = sync_user_devices(&mut alice.db, &alice.keys, SERVER, ub, &forged, NOW).unwrap();
    assert_eq!(changes.rejected, vec![bob.device]);
    assert!(pinned_devices(&alice.db, SERVER, ub).unwrap().is_empty());
    // Bob's devices listed under Alice's user id are rejected too (binding check).
    let changes = sync_user_devices(
        &mut alice.db,
        &alice.keys,
        SERVER,
        ua,
        &relay.devices_of(ub),
        NOW,
    )
    .unwrap();
    assert_eq!(changes.rejected.len(), 1);
}

#[test]
fn receipts_are_not_stored_as_messages() {
    let (ua, ub, _) = users();
    let mut relay = Relay::default();
    let mut alice = Launcher::new("alice", ua);
    let mut bob = Launcher::new("bob", ub);
    relay.register(&mut alice);
    relay.register(&mut bob);
    bob.learn(&relay, &[ua]);
    alice.learn(&relay, &[ub]);
    let conv = Uuid::now_v7();
    let up_to = Uuid::now_v7();
    let receipt = Payload {
        v: 1,
        conversation_id: conv,
        client_message_id: Uuid::now_v7(),
        sent_at: OffsetDateTime::UNIX_EPOCH,
        content: Content::ReceiptRead { up_to },
    };
    let claimed = relay.claim(&[bob.device]);
    let out = encrypt_for(
        &mut alice.db,
        &alice.keys,
        SERVER,
        &[bob.device],
        &claimed,
        &receipt.encode().unwrap(),
        NOW,
    )
    .unwrap();
    relay.deliver(&alice, conv, &out.envelopes);
    let got = bob.receive_all(&mut relay);
    assert_eq!(
        got[0],
        Received::Receipt {
            conversation_id: conv,
            sender_user_id: ua,
            up_to
        }
    );
    assert!(
        list_messages(&bob.db, &bob.keys, SERVER, conv, None, 10)
            .unwrap()
            .is_empty()
    );
}

/// No-panic property for the whole receive path (01-security §9): envelopes come from the server.
mod no_panic {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        #[test]
        fn receive_arbitrary_envelopes(
            kind in any::<u8>(),
            ciphertext in prop_oneof![
                "[A-Za-z0-9+/=]{0,400}",
                proptest::collection::vec(any::<u8>(), 0..300).prop_map(|b| crypto::wire_b64(&b)),
                proptest::collection::vec(any::<char>(), 0..200).prop_map(|c| c.into_iter().collect::<String>()),
            ],
            wrong_identity in any::<bool>(),
        ) {
            let (ua, ub, _) = users();
            let mut relay = Relay::default();
            let mut alice = Launcher::new("alice", ua);
            let mut bob = Launcher::new("bob", ub);
            relay.register(&mut alice);
            relay.register(&mut bob);
            bob.learn(&relay, &[ua]);
            let identity = if wrong_identity { "x".repeat(43) } else { relay.devices[&alice.device].1.identity_key.clone() };
            let env = InboxEnvelope {
                id: Uuid::now_v7(),
                conversation_id: Uuid::now_v7(),
                sender_user_id: ua,
                sender_device_id: alice.device,
                sender_identity_key: identity,
                algorithm: "olm.v1".into(),
                olm_message_type: kind,
                ciphertext,
                created_at: OffsetDateTime::UNIX_EPOCH,
            };
            prop_assert!(receive(&mut bob.db, &bob.keys, SERVER, ub, &env, NOW).is_err());
        }
    }
}

#[test]
fn oversized_ciphertext_is_refused_before_decoding() {
    let (ua, ub, _) = users();
    let mut relay = Relay::default();
    let mut alice = Launcher::new("alice", ua);
    let mut bob = Launcher::new("bob", ub);
    relay.register(&mut alice);
    relay.register(&mut bob);
    bob.learn(&relay, &[ua]);
    let env = InboxEnvelope {
        id: Uuid::now_v7(),
        conversation_id: Uuid::now_v7(),
        sender_user_id: ua,
        sender_device_id: alice.device,
        sender_identity_key: relay.devices[&alice.device].1.identity_key.clone(),
        algorithm: "olm.v1".into(),
        olm_message_type: 1,
        ciphertext: crypto::wire_b64(&vec![
            0u8;
            vgames_proto::social::MAX_ENVELOPE_CIPHERTEXT + 1
        ]),
        created_at: OffsetDateTime::UNIX_EPOCH,
    };
    assert!(matches!(
        receive(&mut bob.db, &bob.keys, SERVER, ub, &env, NOW),
        Err(ReceiveError::Undecryptable(CryptoError::BadMessage))
    ));
}

// ---- A4-T08: key top-up, account replacement, targets, conversation cache -------------------

#[test]
fn fallback_key_ids_never_collide_with_one_time_key_ids() {
    let mut l = Launcher::new("ana", Uuid::now_v7());
    set_device_id(&l.db, SERVER, Uuid::now_v7(), NOW).unwrap();
    let up = prepare_key_upload(&mut l.db, &l.keys, SERVER, 50, true, NOW).unwrap();
    let fallback = up.fallback_key.unwrap();
    assert!(fallback.key_id.starts_with(crypto::FALLBACK_KEY_ID_PREFIX));
    let mut ids: Vec<&str> = up.one_time_keys.iter().map(|k| k.key_id.as_str()).collect();
    ids.push(&fallback.key_id);
    let unique: std::collections::HashSet<&str> = ids.iter().copied().collect();
    assert_eq!(unique.len(), 51, "{ids:?}");
    // The prefixed id is what the fallback signature covers.
    let info = account_info(&l.db, &l.keys, SERVER).unwrap().unwrap();
    crypto::verify_one_time_key(&info.signing_key, &claimed(Uuid::nil(), &fallback, true)).unwrap();
    // A peer still opens a session with it (the id is only a label).
    let (ub, _, _) = users();
    let mut relay = Relay::default();
    let mut bob = Launcher::new("bob", ub);
    let mut ana = Launcher::new("ana2", l.user);
    relay.register(&mut ana);
    relay.register(&mut bob);
    relay.otks.get_mut(&ana.device).unwrap().clear();
    let conv = Uuid::now_v7();
    bob.send(&mut relay, conv, &[ana.user], "on the fallback key");
    ana.learn(&relay, &[ub]);
    assert_eq!(
        text_of(&ana.receive_all(&mut relay)[0]),
        "on the fallback key"
    );
}

#[test]
fn top_up_refills_to_fifty_and_resends_unpublished_keys() {
    let mut l = Launcher::new("ana", Uuid::now_v7());
    set_device_id(&l.db, SERVER, Uuid::now_v7(), NOW).unwrap();
    let first = prepare_top_up(&mut l.db, &l.keys, SERVER, 0, true, NOW).unwrap();
    assert_eq!(first.one_time_keys.len(), crypto::INITIAL_ONE_TIME_KEYS);
    assert!(first.fallback_key.is_some());
    // The upload failed: the same keys go again, nothing new is generated.
    let again = prepare_top_up(&mut l.db, &l.keys, SERVER, 0, false, NOW).unwrap();
    assert_eq!(again.one_time_keys, first.one_time_keys);
    mark_keys_published(&mut l.db, &l.keys, SERVER, NOW).unwrap();
    // 45 left on the server: 5 more.
    let more = prepare_top_up(&mut l.db, &l.keys, SERVER, 45, false, NOW).unwrap();
    assert_eq!(more.one_time_keys.len(), 5);
    assert!(more.fallback_key.is_none());
    mark_keys_published(&mut l.db, &l.keys, SERVER, NOW).unwrap();
    // Enough on the server: nothing.
    let none = prepare_top_up(&mut l.db, &l.keys, SERVER, 60, false, NOW).unwrap();
    assert!(none.one_time_keys.is_empty() && none.fallback_key.is_none());
    // Never above the server's cap of 100 unclaimed keys.
    let capped = prepare_top_up(&mut l.db, &l.keys, SERVER, 99, false, NOW).unwrap();
    assert!(capped.one_time_keys.is_empty());
}

#[test]
fn replacing_the_account_keeps_history_and_drops_sessions() {
    let (ua, ub, _) = users();
    let mut relay = Relay::default();
    let mut alice = Launcher::new("alice", ua);
    let mut bob = Launcher::new("bob", ub);
    relay.register(&mut alice);
    relay.register(&mut bob);
    let conv = Uuid::now_v7();
    alice.send(&mut relay, conv, &[ub], "before");
    bob.learn(&relay, &[ua]);
    bob.receive_all(&mut relay);
    let old = account_info(&bob.db, &bob.keys, SERVER).unwrap().unwrap();

    let new = replace_account(&mut bob.db, &bob.keys, SERVER, NOW).unwrap();
    assert_eq!(new.user_id, ub);
    assert_eq!(new.device_id, None);
    assert_ne!(new.identity_key, old.identity_key);
    assert_ne!(new.signing_key, old.signing_key);
    let sessions: i64 = bob
        .db
        .query_row("SELECT count(*) FROM social_olm_sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sessions, 0);
    let history = list_messages(&bob.db, &bob.keys, SERVER, conv, None, 10).unwrap();
    assert_eq!(history.len(), 1, "history stays readable");
    // Registered again, Bob receives on the new account.
    relay.devices.retain(|_, (u, _)| *u != ub);
    relay.register(&mut bob);
    alice.send(&mut relay, conv, &[ub], "after");
    assert_eq!(text_of(&bob.receive_all(&mut relay)[0]), "after");
}

#[test]
fn targets_split_trusted_and_changed_devices_and_skip_this_one() {
    let (ua, ub, _) = users();
    let mut relay = Relay::default();
    let mut alice = Launcher::new("alice", ua);
    let mut alice2 = Launcher::second_device("alice2", &alice);
    let mut bob = Launcher::new("bob", ub);
    relay.register(&mut alice);
    relay.register(&mut alice2);
    relay.register(&mut bob);
    let carol = Uuid::now_v7();

    let t = targets(&alice.db, &alice.keys, SERVER, &[ua, ub, carol]).unwrap();
    assert_eq!(t.never_synced, vec![ua, ub, carol]);
    alice.learn(&relay, &[ua, ub]);
    let t = targets(&alice.db, &alice.keys, SERVER, &[ua, ub]).unwrap();
    assert_eq!(t.trusted, vec![(ua, alice2.device), (ub, bob.device)]);
    assert!(t.key_changed.is_empty() && t.never_synced.is_empty());

    // Bob's device shows up with other keys: it moves to key_changed.
    let other = Launcher::new("impostor", ub);
    let forged = signed_device_keys(&other.db, &other.keys, SERVER).unwrap();
    let entry = relay.devices.get_mut(&bob.device).unwrap();
    entry.1.identity_key = forged.identity_key;
    entry.1.signing_key = forged.signing_key;
    entry.1.keys_signature = forged.keys_signature;
    alice.learn(&relay, &[ub]);
    let t = targets(&alice.db, &alice.keys, SERVER, &[ua, ub]).unwrap();
    assert_eq!(t.trusted, vec![(ua, alice2.device)]);
    assert_eq!(t.key_changed, vec![(ub, bob.device)]);
}

fn summary(id: Uuid, name: &str) -> crate::social::model::UserSummary {
    crate::social::model::UserSummary {
        id,
        username: name.to_owned(),
        display_name: None,
        avatar_url: None,
    }
}

#[test]
fn the_conversation_cache_orders_counts_unread_and_finds_members() {
    use crate::social::model::ConversationKind;
    let (ua, ub, uc) = users();
    let mut relay = Relay::default();
    let mut alice = Launcher::new("alice", ua);
    let mut bob = Launcher::new("bob", ub);
    relay.register(&mut alice);
    relay.register(&mut bob);
    let direct = Uuid::now_v7();
    let party = Uuid::now_v7();
    let list = vec![
        CachedConversation {
            id: direct,
            kind: ConversationKind::Direct,
            members: vec![summary(ua, "alice"), summary(ub, "bob")],
            created_at: NOW - 100,
            last_activity_at: NOW - 100,
        },
        CachedConversation {
            id: party,
            kind: ConversationKind::Party,
            members: vec![
                summary(ua, "alice"),
                summary(ub, "bob"),
                summary(uc, "carol"),
            ],
            created_at: NOW - 50,
            last_activity_at: NOW - 50,
        },
    ];
    replace_conversations(&mut bob.db, SERVER, &list).unwrap();
    let got = list_conversations(&bob.db, &bob.keys, SERVER).unwrap();
    assert_eq!(
        got.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![party, direct]
    );
    assert!(
        got.iter()
            .all(|c| c.unread == 0 && c.last_message.is_none())
    );

    // Two messages in the direct conversation move it up and count as unread.
    alice.send(&mut relay, direct, &[ub], "one");
    alice.send(&mut relay, direct, &[ub], "two");
    bob.learn(&relay, &[ua]);
    bob.receive_all(&mut relay);
    let got = list_conversations(&bob.db, &bob.keys, SERVER).unwrap();
    assert_eq!(got[0].id, direct);
    assert_eq!(got[0].unread, 2);
    assert_eq!(
        got[0].last_message.as_ref().map(|m| m.body.clone()),
        Some(MessageBody::Text { text: "two".into() })
    );
    assert!(mark_read(&bob.db, SERVER, direct).unwrap());
    assert_eq!(
        get_conversation(&bob.db, &bob.keys, SERVER, direct)
            .unwrap()
            .unwrap()
            .unread,
        0
    );
    // Own messages never count as unread.
    bob.send(&mut relay, direct, &[ua], "three");
    assert_eq!(
        get_conversation(&bob.db, &bob.keys, SERVER, direct)
            .unwrap()
            .unwrap()
            .unread,
        0
    );

    let (kind, members) = conversation_members(&bob.db, SERVER, party)
        .unwrap()
        .unwrap();
    assert_eq!(kind, ConversationKind::Party);
    assert_eq!(members.len(), 3);
    let mut with_carol = conversations_with(&bob.db, SERVER, uc).unwrap();
    with_carol.sort();
    assert_eq!(with_carol, vec![party]);
    assert_eq!(conversations_with(&bob.db, SERVER, ua).unwrap().len(), 2);

    // Left the party: it leaves the list, its history stays.
    replace_conversations(&mut bob.db, SERVER, &list[..1]).unwrap();
    assert!(!is_cached(&bob.db, SERVER, party).unwrap());
    assert!(is_cached(&bob.db, SERVER, direct).unwrap());
    assert!(!mark_read(&bob.db, SERVER, party).unwrap());
    assert_eq!(
        list_conversations(&bob.db, &bob.keys, SERVER)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        list_messages(&bob.db, &bob.keys, SERVER, direct, None, 10)
            .unwrap()
            .len(),
        3
    );
}
