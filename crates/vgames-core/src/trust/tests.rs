use super::*;

pub(crate) const SERVER: &str = "01920000-0000-7000-8000-000000000000";
pub(crate) const ALICE: &str = "0192aaaa-0000-7000-8000-000000000001";

pub(crate) fn server() -> Uuid {
    SERVER.parse().unwrap()
}

pub(crate) fn key(n: u8) -> SecretKey {
    SecretKey::from_seed(&[n; 32])
}

pub(crate) fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

pub(crate) fn publisher(k: &SecretKey, holder: &str) -> PublisherKey {
    PublisherKey {
        key_id: k.public_key().key_id(),
        public_key: k.public_key(),
        holder_user_id: holder.parse().unwrap(),
        label: "alice@workstation".into(),
        not_before: ts("2026-09-24T00:00:00Z"),
        not_after: ts("2028-09-24T00:00:00Z"),
    }
}

pub(crate) fn bundle(root: &SecretKey, version: u64, publishers: Vec<PublisherKey>) -> TrustBundle {
    TrustBundle {
        format: FORMAT.into(),
        server_id: server(),
        version,
        issued_at: ts("2026-09-24T10:00:00Z"),
        expires_at: None,
        root_key_id: root.public_key().key_id(),
        publishers,
        revoked: vec![],
        next_root: None,
    }
}

pub(crate) fn signed(root: &SecretKey, b: &TrustBundle) -> (Vec<u8>, Signature) {
    let bytes = b.to_bytes();
    let sig = sign_bundle(root, &bytes);
    (bytes, sig)
}

fn verify(
    root: &SecretKey,
    b: &TrustBundle,
    pin: &RootPin,
    last: Option<u64>,
) -> Result<VerifiedBundle, TrustError> {
    let (bytes, sig) = signed(root, b);
    verify_bundle(&bytes, &sig, pin, last, server())
}

fn pin(k: &SecretKey) -> RootPin {
    RootPin::new(k.public_key())
}

#[test]
fn valid_bundle_builds_trust_state() {
    let root = key(1);
    let (alice, bob) = (key(2), key(3));
    let mut b = bundle(
        &root,
        4,
        vec![publisher(&alice, ALICE), publisher(&bob, ALICE)],
    );
    b.revoked.push(Revocation {
        key_id: bob.public_key().key_id(),
        revoked_at: ts("2026-10-01T12:00:00Z"),
        reason: "laptop stolen".into(),
    });
    let v = verify(&root, &b, &pin(&root), Some(3)).unwrap();
    assert!(v.newer && !v.rotated);
    assert_eq!(v.pin, pin(&root));
    assert_eq!(v.state.version(), 4);
    let a = alice.public_key().key_id();
    assert!(matches!(v.state.key_status(&a), KeyStatus::Trusted(p) if p.key_id == a));
    // Revocation wins over listing.
    assert_eq!(
        v.state.key_status(&bob.public_key().key_id()),
        KeyStatus::Revoked
    );
    assert_eq!(
        v.state.key_status(&key(9).public_key().key_id()),
        KeyStatus::Unknown
    );
    assert!(v.state.publisher(&bob.public_key().key_id()).is_some());
}

#[test]
fn signature_must_come_from_the_pinned_root() {
    let b = bundle(&key(1), 1, vec![]);
    // Signed by another key.
    let (bytes, _) = signed(&key(1), &b);
    let sig = sign_bundle(&key(7), &bytes);
    assert_eq!(
        verify_bundle(&bytes, &sig, &pin(&key(1)), None, server()),
        Err(TrustError::Signature)
    );
    // Tampered bytes.
    let (mut bytes, sig) = signed(&key(1), &b);
    let last = bytes.len() - 2;
    bytes[last] ^= 1;
    assert_eq!(
        verify_bundle(&bytes, &sig, &pin(&key(1)), None, server()),
        Err(TrustError::Signature)
    );
    // Signed with the manifest context instead of the trust context.
    let (bytes, _) = signed(&key(1), &b);
    let sig = key(1).sign(Context::Manifest, &bytes);
    assert_eq!(
        verify_bundle(&bytes, &sig, &pin(&key(1)), None, server()),
        Err(TrustError::Signature)
    );
}

#[test]
fn root_key_id_must_name_the_signer() {
    let mut b = bundle(&key(1), 1, vec![]);
    b.root_key_id = key(5).public_key().key_id();
    assert_eq!(
        verify(&key(1), &b, &pin(&key(1)), None),
        Err(TrustError::RootKeyId)
    );
}

#[test]
fn server_id_must_match() {
    let mut b = bundle(&key(1), 1, vec![]);
    b.server_id = ALICE.parse().unwrap();
    assert!(matches!(
        verify(&key(1), &b, &pin(&key(1)), None),
        Err(TrustError::ServerId { .. })
    ));
}

#[test]
fn version_never_goes_down() {
    let b = bundle(&key(1), 4, vec![]);
    assert_eq!(
        verify(&key(1), &b, &pin(&key(1)), Some(5)),
        Err(TrustError::Rollback {
            last_seen: 5,
            found: 4
        })
    );
    // The same version is a refresh, not an update.
    let v = verify(&key(1), &b, &pin(&key(1)), Some(4)).unwrap();
    assert!(!v.newer);
    assert_eq!(
        verify(&key(1), &bundle(&key(1), 0, vec![]), &pin(&key(1)), None),
        Err(TrustError::ZeroVersion)
    );
}

#[test]
fn expiry_is_reported_not_enforced_here() {
    let mut b = bundle(&key(1), 1, vec![]);
    b.expires_at = Some(ts("2026-10-01T00:00:00Z"));
    let v = verify(&key(1), &b, &pin(&key(1)), None).unwrap();
    assert!(!v.state.is_expired(ts("2026-09-30T23:59:59Z")));
    assert!(v.state.is_expired(ts("2026-10-01T00:00:00Z")));
    b.expires_at = Some(b.issued_at);
    assert_eq!(
        verify(&key(1), &b, &pin(&key(1)), None),
        Err(TrustError::Expiry)
    );
}

#[test]
fn entry_rules() {
    let root = key(1);
    let mut p = publisher(&key(2), ALICE);
    p.key_id = key(3).public_key().key_id();
    let r = verify(&root, &bundle(&root, 1, vec![p]), &pin(&root), None);
    assert_eq!(
        r,
        Err(TrustError::Publisher {
            index: 0,
            fault: EntryFault::KeyId
        })
    );

    let p = publisher(&key(2), ALICE);
    let r = verify(
        &root,
        &bundle(&root, 1, vec![p.clone(), p]),
        &pin(&root),
        None,
    );
    assert_eq!(
        r,
        Err(TrustError::Publisher {
            index: 1,
            fault: EntryFault::Duplicate
        })
    );

    let r = verify(
        &root,
        &bundle(&root, 1, vec![publisher(&root, ALICE)]),
        &pin(&root),
        None,
    );
    assert!(matches!(
        r,
        Err(TrustError::Publisher {
            fault: EntryFault::RootAsPublisher,
            ..
        })
    ));

    let mut p = publisher(&key(2), ALICE);
    p.not_after = p.not_before;
    let r = verify(&root, &bundle(&root, 1, vec![p]), &pin(&root), None);
    assert!(matches!(
        r,
        Err(TrustError::Publisher {
            fault: EntryFault::Window,
            ..
        })
    ));

    let mut p = publisher(&key(2), ALICE);
    p.label = "a\u{1b}[31m".into();
    let r = verify(&root, &bundle(&root, 1, vec![p]), &pin(&root), None);
    assert!(matches!(
        r,
        Err(TrustError::Publisher {
            fault: EntryFault::Label,
            ..
        })
    ));

    let mut b = bundle(&root, 1, vec![]);
    let rev = Revocation {
        key_id: key(2).public_key().key_id(),
        revoked_at: ts("2026-10-01T00:00:00Z"),
        reason: String::new(),
    };
    b.revoked = vec![rev.clone(), rev];
    assert!(matches!(
        verify(&root, &b, &pin(&root), None),
        Err(TrustError::Revoked {
            index: 1,
            fault: EntryFault::Duplicate
        })
    ));
}

#[test]
fn strict_json() {
    let root = key(1);
    let b = bundle(&root, 1, vec![]);
    let mut v = serde_json::to_value(&b).unwrap();
    v["extra"] = serde_json::json!(true);
    let bytes = serde_json::to_vec(&v).unwrap();
    let sig = sign_bundle(&root, &bytes);
    assert!(matches!(
        verify_bundle(&bytes, &sig, &pin(&root), None, server()),
        Err(TrustError::Json(_))
    ));
    let mut v = serde_json::to_value(&b).unwrap();
    v["format"] = "vgames.trust/2".into();
    let bytes = serde_json::to_vec(&v).unwrap();
    let sig = sign_bundle(&root, &bytes);
    assert!(matches!(
        verify_bundle(&bytes, &sig, &pin(&root), None, server()),
        Err(TrustError::Format(_))
    ));
    let big = vec![b' '; MAX_BUNDLE_BYTES + 1];
    assert_eq!(
        verify_bundle(&big, &sign_bundle(&root, &big), &pin(&root), None, server()),
        Err(TrustError::TooLarge)
    );
}

#[test]
fn rotation_chain() {
    let (r1, r2, r3) = (key(1), key(11), key(21));
    let next = |k: &SecretKey| NextRoot {
        public_key: k.public_key(),
        key_id: k.public_key().key_id(),
    };

    // v1 by r1: plain.
    let v1 = verify(&r1, &bundle(&r1, 1, vec![]), &pin(&r1), None).unwrap();
    assert_eq!(v1.pin, pin(&r1));

    // v2 by r1 announces r2: the pin records the pending rotation.
    let mut b2 = bundle(&r1, 2, vec![]);
    b2.next_root = Some(next(&r2));
    let v2 = verify(&r1, &b2, &v1.pin, Some(1)).unwrap();
    assert!(!v2.rotated);
    assert_eq!(v2.pin.root, r1.public_key());
    assert_eq!(v2.pin.next_root, Some(r2.public_key()));
    // The cached v2 still re-verifies after the announcement.
    verify(&r1, &b2, &v2.pin, Some(2)).unwrap();

    // v3 by r2 completes the rotation: the pin moves to r2.
    let v3 = verify(&r2, &bundle(&r2, 3, vec![]), &v2.pin, Some(2)).unwrap();
    assert!(v3.rotated);
    assert_eq!(v3.pin, pin(&r2));

    // The old root is no longer accepted, even for a newer version.
    assert_eq!(
        verify(&r1, &bundle(&r1, 4, vec![]), &v3.pin, Some(3)),
        Err(TrustError::Signature)
    );
    // A key that was never announced cannot take over.
    assert_eq!(
        verify(&r3, &bundle(&r3, 4, vec![]), &v3.pin, Some(3)),
        Err(TrustError::Signature)
    );
    // Without an announcement there is no other rotation path.
    assert_eq!(
        verify(&r2, &bundle(&r2, 2, vec![]), &v1.pin, Some(1)),
        Err(TrustError::Signature)
    );
    // Rotation does not bypass the version rule.
    assert!(matches!(
        verify(&r2, &bundle(&r2, 1, vec![]), &v2.pin, Some(2)),
        Err(TrustError::Rollback { .. })
    ));
}

#[test]
fn next_root_rules() {
    let r1 = key(1);
    let mut b = bundle(&r1, 1, vec![]);
    b.next_root = Some(NextRoot {
        public_key: r1.public_key(),
        key_id: r1.public_key().key_id(),
    });
    assert_eq!(
        verify(&r1, &b, &pin(&r1), None),
        Err(TrustError::NextRoot(EntryFault::SameRoot))
    );
    b.next_root = Some(NextRoot {
        public_key: key(2).public_key(),
        key_id: key(3).public_key().key_id(),
    });
    assert_eq!(
        verify(&r1, &b, &pin(&r1), None),
        Err(TrustError::NextRoot(EntryFault::KeyId))
    );
}

#[test]
fn signed_bundle_wire_format() {
    let root = key(1);
    let (bytes, sig) = signed(&root, &bundle(&root, 1, vec![publisher(&key(2), ALICE)]));
    let wire = serde_json::to_string(&SignedBundle::new(&bytes, sig)).unwrap();
    let back: SignedBundle = serde_json::from_str(&wire).unwrap();
    let decoded = back.bundle_bytes().unwrap();
    assert_eq!(decoded, bytes);
    verify_bundle(&decoded, &back.signature, &pin(&root), None, server()).unwrap();
    insta::assert_snapshot!(String::from_utf8(bytes).unwrap());
}
