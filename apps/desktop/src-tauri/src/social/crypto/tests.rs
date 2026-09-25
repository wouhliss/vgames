//! A4-T02 acceptance at the primitive level: two and three in-process accounts.

use super::*;

const SERVER: Uuid = Uuid::from_u128(0x01920000_0000_7000_8000_00000000abcd);

fn uid(n: u128) -> Uuid {
    Uuid::from_u128(0x01920000_0000_7000_8000_000000000000 | n)
}

fn key() -> SecretKey32 {
    SecretKey32::generate().unwrap()
}

/// What the server would return from `GET /v1/users/{id}/devices`.
fn device_keys(account: &OlmAccount, user: Uuid, device: Uuid) -> DeviceKeys {
    let signed = account.signed_device_keys(user, SERVER);
    DeviceKeys {
        device_id: device,
        display_name: Some("test".into()),
        identity_key: signed.identity_key,
        signing_key: signed.signing_key,
        keys_signature: signed.keys_signature,
        created_at: time::OffsetDateTime::UNIX_EPOCH,
    }
}

/// Publishes `n` one-time keys (and a fallback key) and returns them as the server stores them.
fn publish(account: &mut OlmAccount, n: usize) -> (Vec<SignedOneTimeKey>, SignedOneTimeKey) {
    account.generate_one_time_keys(n);
    account.generate_fallback_key();
    let (otks, fallback) = account.unpublished_keys();
    account.mark_keys_as_published();
    (otks, fallback.unwrap())
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

struct Party {
    user: Uuid,
    device: Uuid,
    account: OlmAccount,
}

impl Party {
    fn new(n: u128) -> Self {
        Self {
            user: uid(n),
            device: uid(0x100 + n),
            account: OlmAccount::new(),
        }
    }

    fn pinned(&self) -> PinnedDevice {
        verify_device_keys(
            &device_keys(&self.account, self.user, self.device),
            self.user,
            SERVER,
        )
        .unwrap()
    }
}

#[test]
fn device_self_signature_binds_user_and_server() {
    let alice = Party::new(1);
    let keys = device_keys(&alice.account, alice.user, alice.device);
    assert!(verify_device_keys(&keys, alice.user, SERVER).is_ok());
    assert!(matches!(
        verify_device_keys(&keys, uid(2), SERVER),
        Err(CryptoError::WrongBinding)
    ));
    assert!(matches!(
        verify_device_keys(&keys, alice.user, uid(9)),
        Err(CryptoError::WrongBinding)
    ));
    // Another account's identity key under Alice's signature.
    let mut swapped = keys.clone();
    swapped.identity_key = OlmAccount::new().identity_key();
    assert!(verify_device_keys(&swapped, alice.user, SERVER).is_err());
    let mut garbage = keys;
    garbage.keys_signature = "A".repeat(85);
    assert!(verify_device_keys(&garbage, alice.user, SERVER).is_err());
}

#[test]
fn one_time_key_signatures_cover_the_fallback_flag() {
    let mut bob = Party::new(2);
    let (otks, fallback) = publish(&mut bob.account, 3);
    assert_eq!(otks.len(), 3);
    let signing = bob.account.signing_key();
    for k in &otks {
        verify_one_time_key(&signing, &claimed(bob.device, k, false)).unwrap();
        // A server relabelling a one-time key as a reusable fallback key is detected.
        assert!(verify_one_time_key(&signing, &claimed(bob.device, k, true)).is_err());
    }
    verify_one_time_key(&signing, &claimed(bob.device, &fallback, true)).unwrap();
    assert!(verify_one_time_key(&signing, &claimed(bob.device, &fallback, false)).is_err());
    // Someone else's signing key.
    assert!(
        verify_one_time_key(
            &OlmAccount::new().signing_key(),
            &claimed(bob.device, &otks[0], false)
        )
        .is_err()
    );
    // Published keys are no longer "unpublished".
    let (again, fb) = bob.account.unpublished_keys();
    assert!(again.is_empty() && fb.is_none());
}

/// Alice → Bob establishes a session with a pre-key message; after Bob replies, both sides
/// send normal messages.
#[test]
fn pre_key_then_normal_messages() {
    let alice = Party::new(1);
    let mut bob = Party::new(2);
    let (otks, _) = publish(&mut bob.account, 5);

    let mut a_to_b = alice
        .account
        .create_outbound_session(&bob.pinned(), &claimed(bob.device, &otks[0], false))
        .unwrap();
    let (t1, m1) = a_to_b.encrypt(b"hello bob").unwrap();
    let (t2, m2) = a_to_b.encrypt(b"are you there").unwrap();
    assert_eq!((t1, t2), (MESSAGE_TYPE_PRE_KEY, MESSAGE_TYPE_PRE_KEY));

    let msg1 = parse_message(t1, &m1).unwrap();
    assert_eq!(pre_key_session_id(&msg1).unwrap(), a_to_b.session_id());
    let (mut b_from_a, plain) = bob
        .account
        .create_inbound_session(&alice.account.identity_key(), &msg1)
        .unwrap();
    assert_eq!(plain, b"hello bob");
    assert_eq!(b_from_a.session_id(), a_to_b.session_id());
    // The second pre-key message belongs to the same session.
    let msg2 = parse_message(t2, &m2).unwrap();
    assert_eq!(pre_key_session_id(&msg2).unwrap(), b_from_a.session_id());
    assert_eq!(b_from_a.decrypt(&msg2).unwrap(), b"are you there");
    // The one-time key is used up: replaying the first pre-key message cannot create a session.
    assert!(matches!(
        bob.account
            .create_inbound_session(&alice.account.identity_key(), &msg1),
        Err(CryptoError::UnknownOneTimeKey)
    ));

    let (t3, m3) = b_from_a.encrypt(b"yes").unwrap();
    assert_eq!(t3, MESSAGE_TYPE_NORMAL);
    assert_eq!(
        a_to_b.decrypt(&parse_message(t3, &m3).unwrap()).unwrap(),
        b"yes"
    );
    let (t4, m4) = a_to_b.encrypt(b"great").unwrap();
    assert_eq!(
        t4, MESSAGE_TYPE_NORMAL,
        "Alice switches to normal messages once Bob answered"
    );
    assert_eq!(
        b_from_a.decrypt(&parse_message(t4, &m4).unwrap()).unwrap(),
        b"great"
    );
}

#[test]
fn out_of_order_delivery_decrypts_and_duplicates_fail_closed() {
    let alice = Party::new(1);
    let mut bob = Party::new(2);
    let (otks, _) = publish(&mut bob.account, 1);
    let mut a = alice
        .account
        .create_outbound_session(&bob.pinned(), &claimed(bob.device, &otks[0], false))
        .unwrap();
    let (t0, m0) = a.encrypt(b"m0").unwrap();
    let (mut b, _) = bob
        .account
        .create_inbound_session(
            &alice.account.identity_key(),
            &parse_message(t0, &m0).unwrap(),
        )
        .unwrap();
    let (tr, mr) = b.encrypt(b"ack").unwrap();
    a.decrypt(&parse_message(tr, &mr).unwrap()).unwrap();

    let sent: Vec<(u8, Vec<u8>)> = (1..=6)
        .map(|i| a.encrypt(format!("m{i}").as_bytes()).unwrap())
        .collect();
    for i in [3usize, 1, 6, 2, 5, 4] {
        let (t, m) = &sent[i - 1];
        let plain = b.decrypt(&parse_message(*t, m).unwrap()).unwrap();
        assert_eq!(plain, format!("m{i}").as_bytes());
    }
    // Delivering a message twice fails: the message key is gone. The engine therefore
    // de-duplicates by envelope id and (sender device, client_message_id) before decrypting.
    let (t, m) = &sent[0];
    assert!(matches!(
        b.decrypt(&parse_message(*t, m).unwrap()),
        Err(CryptoError::Decrypt)
    ));
}

#[test]
fn tampered_ciphertext_is_rejected() {
    let alice = Party::new(1);
    let mut bob = Party::new(2);
    let (otks, _) = publish(&mut bob.account, 1);
    let mut a = alice
        .account
        .create_outbound_session(&bob.pinned(), &claimed(bob.device, &otks[0], false))
        .unwrap();
    let (t, mut m) = a.encrypt(b"secret").unwrap();
    let last = m.len() - 12;
    m[last] ^= 0x01;
    let parsed = parse_message(t, &m);
    let result = parsed.and_then(|msg| {
        bob.account
            .create_inbound_session(&alice.account.identity_key(), &msg)
            .map(|_| ())
    });
    assert!(result.is_err(), "a flipped bit must not decrypt");
    // The one-time key was not consumed by the forgery: the real message still works.
    let (t2, m2) = a.encrypt(b"secret").unwrap();
    let (_, plain) = bob
        .account
        .create_inbound_session(
            &alice.account.identity_key(),
            &parse_message(t2, &m2).unwrap(),
        )
        .unwrap();
    assert_eq!(plain, b"secret");
}

/// Once every one-time key is claimed, the server hands out the fallback key; several senders
/// may use it at once.
#[test]
fn fallback_key_after_one_time_keys_are_exhausted() {
    let alice = Party::new(1);
    let carol = Party::new(3);
    let mut bob = Party::new(2);
    let (otks, fallback) = publish(&mut bob.account, 1);

    // Alice took the only one-time key.
    let mut a = alice
        .account
        .create_outbound_session(&bob.pinned(), &claimed(bob.device, &otks[0], false))
        .unwrap();
    // Carol gets the fallback key, and so does Alice's second device.
    let mut c = carol
        .account
        .create_outbound_session(&bob.pinned(), &claimed(bob.device, &fallback, true))
        .unwrap();
    let alice2 = Party::new(4);
    let mut a2 = alice2
        .account
        .create_outbound_session(&bob.pinned(), &claimed(bob.device, &fallback, true))
        .unwrap();

    for (sender, session, text) in [
        (&alice, &mut a, "from alice"),
        (&carol, &mut c, "from carol"),
        (&alice2, &mut a2, "from alice's laptop"),
    ] {
        let (t, m) = session.encrypt(text.as_bytes()).unwrap();
        let (_, plain) = bob
            .account
            .create_inbound_session(
                &sender.account.identity_key(),
                &parse_message(t, &m).unwrap(),
            )
            .unwrap();
        assert_eq!(plain, text.as_bytes());
    }

    // After a rotation the previous fallback key still works for in-flight messages.
    bob.account.generate_fallback_key();
    let dave = Party::new(5);
    let mut d = dave
        .account
        .create_outbound_session(&bob.pinned(), &claimed(bob.device, &fallback, true))
        .unwrap();
    let (t, m) = d.encrypt(b"late").unwrap();
    assert!(
        bob.account
            .create_inbound_session(&dave.account.identity_key(), &parse_message(t, &m).unwrap())
            .is_ok()
    );
}

#[test]
fn pickles_round_trip_and_need_the_right_key() {
    let k = key();
    let alice = Party::new(1);
    let mut bob = Party::new(2);
    let (otks, _) = publish(&mut bob.account, 2);

    let mut a = alice
        .account
        .create_outbound_session(&bob.pinned(), &claimed(bob.device, &otks[0], false))
        .unwrap();
    let (t, m) = a.encrypt(b"one").unwrap();

    // Bob restarts: his account comes back from the encrypted pickle.
    let pickled = bob.account.pickle(&k);
    assert!(
        !pickled.contains(&bob.account.identity_key()),
        "pickles are encrypted"
    );
    assert!(OlmAccount::unpickle(&pickled, &key()).is_err(), "wrong key");
    let mut bob_restored = OlmAccount::unpickle(&pickled, &k).unwrap();
    assert_eq!(bob_restored.identity_key(), bob.account.identity_key());
    assert_eq!(bob_restored.signing_key(), bob.account.signing_key());
    let (b, plain) = bob_restored
        .create_inbound_session(
            &alice.account.identity_key(),
            &parse_message(t, &m).unwrap(),
        )
        .unwrap();
    assert_eq!(plain, b"one");

    // Both sessions survive a pickle round trip mid-conversation.
    let mut b = OlmSession::unpickle(&b.pickle(&k), &k).unwrap();
    let mut a = OlmSession::unpickle(&a.pickle(&k), &k).unwrap();
    assert!(OlmSession::unpickle(&a.pickle(&k), &key()).is_err());
    let (t, m) = b.encrypt(b"two").unwrap();
    assert_eq!(a.decrypt(&parse_message(t, &m).unwrap()).unwrap(), b"two");
    let (t, m) = a.encrypt(b"three").unwrap();
    assert_eq!(b.decrypt(&parse_message(t, &m).unwrap()).unwrap(), b"three");
}

#[test]
fn safety_numbers_are_symmetric_and_change_with_devices() {
    let alice = Party::new(1);
    let bob = Party::new(2);
    let bob_laptop = OlmAccount::new();
    let keys = |a: &OlmAccount| (a.identity_key(), a.signing_key());

    let a_devices = vec![keys(&alice.account)];
    let b_devices = vec![keys(&bob.account)];
    let from_alice = safety_number((alice.user, &a_devices), (bob.user, &b_devices)).unwrap();
    let from_bob = safety_number((bob.user, &b_devices), (alice.user, &a_devices)).unwrap();
    assert_eq!(from_alice, from_bob);
    assert_eq!(from_alice.groups.len(), 12);
    assert!(from_alice.groups.iter().all(|g| g.len() == 5));
    let digits = from_alice.digits();
    assert_eq!(digits.len(), 60);
    assert!(digits.bytes().all(|b| b.is_ascii_digit()));

    // Device order does not matter; a new device changes the number.
    let b_two = vec![keys(&bob_laptop), keys(&bob.account)];
    let b_two_rev = vec![keys(&bob.account), keys(&bob_laptop)];
    let with_laptop = safety_number((alice.user, &a_devices), (bob.user, &b_two)).unwrap();
    assert_eq!(
        with_laptop,
        safety_number((bob.user, &b_two_rev), (alice.user, &a_devices)).unwrap()
    );
    assert_ne!(with_laptop, from_alice);
    // Swapping which user owns which keys changes it too.
    assert_ne!(
        safety_number((alice.user, &b_devices), (bob.user, &a_devices)).unwrap(),
        from_alice
    );
    // Malformed keys are rejected, not hashed.
    let bad = vec![("x".to_string(), "y".to_string())];
    assert!(safety_number((alice.user, &bad), (bob.user, &b_devices)).is_err());
}

#[test]
fn body_cipher_binds_the_row() {
    let cipher = BodyCipher::new(&key()).unwrap();
    let (nonce, ct) = cipher.seal(b"row-1", b"gg wp").unwrap();
    assert_ne!(&ct[..5], b"gg wp");
    assert_eq!(cipher.open(b"row-1", &nonce, &ct).unwrap(), b"gg wp");
    assert!(
        cipher.open(b"row-2", &nonce, &ct).is_err(),
        "moved to another row"
    );
    let mut tampered = ct.clone();
    tampered[0] ^= 1;
    assert!(cipher.open(b"row-1", &nonce, &tampered).is_err());
    assert!(
        BodyCipher::new(&key())
            .unwrap()
            .open(b"row-1", &nonce, &ct)
            .is_err()
    );
    let (n2, _) = cipher.seal(b"row-1", b"gg wp").unwrap();
    assert_ne!(nonce, n2, "fresh nonce per seal");
}

#[test]
fn malformed_input_is_an_error_not_a_panic() {
    assert!(parse_message(0, b"").is_err());
    assert!(parse_message(1, b"\x03garbage").is_err());
    assert!(parse_message(7, b"x").is_err());
    let alice = Party::new(1);
    let bogus = ClaimedKey {
        device_id: uid(9),
        key_id: "AAAAAAAAAAE".into(),
        public_key: "not base64!".into(),
        signature: "x".into(),
        is_fallback: false,
    };
    assert!(
        alice
            .account
            .create_outbound_session(&alice.pinned(), &bogus)
            .is_err()
    );
}
