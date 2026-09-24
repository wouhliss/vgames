use super::*;
use crate::compat::Target;
use crate::layout::PACK_SIZE;
use crate::manifest::tests::build;
use crate::sign::SecretKey;
use crate::trust::tests::{ALICE, bundle, key, publisher, server, signed, ts};
use crate::trust::{Revocation, RootPin, TrustBundle, verify_bundle};

const BOB: &str = "0192bbbb-0000-7000-8000-000000000002";

struct Fixture {
    root: SecretKey,
    publisher: SecretKey,
    manifest: Vec<u8>,
    envelope: Envelope,
    expected: ExpectedRelease,
}

fn fixture() -> Fixture {
    let root = key(1);
    let publisher = key(2);
    let m = build(
        &[("Game/game.exe", 5_000_000), ("Game/data.pak", 10)],
        PACK_SIZE,
    );
    let manifest = serde_json::to_vec(&m).unwrap();
    let envelope = Envelope::sign(&publisher, Context::Manifest, &manifest);
    let expected = ExpectedRelease {
        server_id: m.server_id,
        package_id: m.package_id,
        version_id: m.version_id,
        platform: m.platform,
        sequence: m.sequence,
    };
    Fixture {
        root,
        publisher,
        manifest,
        envelope,
        expected,
    }
}

fn state(f: &Fixture, edit: impl FnOnce(&mut TrustBundle)) -> TrustState {
    let mut b = bundle(&f.root, 1, vec![publisher(&f.publisher, ALICE)]);
    edit(&mut b);
    let (bytes, sig) = signed(&f.root, &b);
    verify_bundle(
        &bytes,
        &sig,
        &RootPin::new(f.root.public_key()),
        None,
        server(),
    )
    .unwrap()
    .state
}

const NOW: &str = "2027-01-01T00:00:00Z";

fn install() -> VerifyMode {
    VerifyMode::Install {
        now: ts(NOW),
        allow_older: false,
    }
}

fn run(f: &Fixture, trust: &TrustState, mode: VerifyMode) -> Result<VerifiedManifest, VerifyError> {
    verify_manifest(trust, &f.envelope, &f.manifest, &f.expected, None, mode)
}

#[test]
fn valid_manifest_verifies_in_every_mode() {
    let f = fixture();
    let t = state(&f, |_| {});
    for mode in [
        install(),
        VerifyMode::Launch,
        VerifyMode::Server {
            now: ts(NOW),
            caller: ALICE.parse().unwrap(),
        },
    ] {
        let v = run(&f, &t, mode).unwrap();
        assert_eq!(v.key_id, f.publisher.public_key().key_id());
        assert_eq!(v.digest, Digest::of(&f.manifest));
        assert_eq!(v.holder_user_id, ALICE.parse::<Uuid>().unwrap());
    }
}

// ---- step 1: trust state ----------------------------------------------------

#[test]
fn step1_trust_bundle_for_another_server() {
    let mut f = fixture();
    let t = state(&f, |_| {});
    f.expected.server_id = BOB.parse().unwrap();
    assert_eq!(
        run(&f, &t, install()),
        Err(VerifyError::TrustServerMismatch)
    );
}

#[test]
fn step1_expired_bundle_blocks_installs_not_launches() {
    let f = fixture();
    let t = state(&f, |b| b.expires_at = Some(ts("2026-12-31T00:00:00Z")));
    assert_eq!(run(&f, &t, install()), Err(VerifyError::TrustExpired));
    run(&f, &t, VerifyMode::Launch).unwrap();
}

// ---- step 2: key in bundle, not revoked -------------------------------------

#[test]
fn step2_unknown_key() {
    let f = fixture();
    let t = state(&f, |b| b.publishers.clear());
    assert!(matches!(
        run(&f, &t, VerifyMode::Launch),
        Err(VerifyError::UnknownKey(_))
    ));
}

#[test]
fn step2_revoked_key_fails_everywhere() {
    let f = fixture();
    let t = state(&f, |b| {
        b.revoked.push(Revocation {
            key_id: f.publisher.public_key().key_id(),
            revoked_at: ts("2026-10-01T12:00:00Z"),
            reason: "laptop stolen".into(),
        })
    });
    for mode in [
        install(),
        VerifyMode::Launch,
        VerifyMode::Server {
            now: ts(NOW),
            caller: ALICE.parse().unwrap(),
        },
    ] {
        assert!(matches!(run(&f, &t, mode), Err(VerifyError::RevokedKey(_))));
    }
}

#[test]
fn step2_expired_key_accepted_by_client_rejected_by_server() {
    let f = fixture();
    let t = state(&f, |b| {
        b.publishers[0].not_after = ts("2026-12-01T00:00:00Z");
    });
    run(&f, &t, install()).unwrap();
    run(&f, &t, VerifyMode::Launch).unwrap();
    assert!(matches!(
        run(
            &f,
            &t,
            VerifyMode::Server {
                now: ts(NOW),
                caller: ALICE.parse().unwrap()
            }
        ),
        Err(VerifyError::KeyNotValidNow(_))
    ));
    // Not yet valid.
    assert!(matches!(
        run(
            &f,
            &t,
            VerifyMode::Server {
                now: ts("2026-01-01T00:00:00Z"),
                caller: ALICE.parse().unwrap()
            }
        ),
        Err(VerifyError::KeyNotValidNow(_))
    ));
}

#[test]
fn step2_server_requires_the_key_holder() {
    let f = fixture();
    let t = state(&f, |_| {});
    assert!(matches!(
        run(
            &f,
            &t,
            VerifyMode::Server {
                now: ts(NOW),
                caller: BOB.parse().unwrap()
            }
        ),
        Err(VerifyError::NotKeyHolder(_))
    ));
}

// ---- step 3: digest and signature -------------------------------------------

#[test]
fn step3_swapped_manifest_bytes() {
    let mut f = fixture();
    let t = state(&f, |_| {});
    // A different (valid) manifest under the original envelope.
    f.manifest = serde_json::to_vec(&build(&[("x", 1)], PACK_SIZE)).unwrap();
    assert_eq!(
        run(&f, &t, install()),
        Err(VerifyError::Signature(
            SignatureError::PayloadDigestMismatch
        ))
    );
}

#[test]
fn step3_forged_envelope_digest() {
    let mut f = fixture();
    let t = state(&f, |_| {});
    // The attacker updates payload_blake3 to match new bytes but cannot re-sign.
    f.manifest = serde_json::to_vec(&build(&[("x", 1)], PACK_SIZE)).unwrap();
    f.envelope.payload_blake3 = Digest::of(&f.manifest);
    assert_eq!(
        run(&f, &t, install()),
        Err(VerifyError::Signature(SignatureError::Invalid))
    );
}

#[test]
fn step3_signature_by_another_key_under_a_trusted_key_id() {
    let mut f = fixture();
    let t = state(&f, |_| {});
    let forged = Envelope::sign(&key(9), Context::Manifest, &f.manifest);
    f.envelope.signature = forged.signature;
    assert_eq!(
        run(&f, &t, install()),
        Err(VerifyError::Signature(SignatureError::Invalid))
    );
}

#[test]
fn step3_cross_context_signature_is_refused() {
    let mut f = fixture();
    let t = state(&f, |_| {});
    // A compat-profile signature over the same bytes.
    f.envelope = Envelope::sign(&f.publisher, Context::Compat, &f.manifest);
    assert_eq!(
        run(&f, &t, install()),
        Err(VerifyError::WrongContext(Context::Compat))
    );
    // Relabelled as a manifest envelope: the domain separation still fails it.
    f.envelope.context = Context::Manifest;
    assert_eq!(
        run(&f, &t, install()),
        Err(VerifyError::Signature(SignatureError::Invalid))
    );
}

// ---- step 4: parse, validate, identity --------------------------------------

#[test]
fn step4_invalid_manifest_even_if_signed() {
    let mut f = fixture();
    let t = state(&f, |_| {});
    let mut m = build(&[("a", 1)], PACK_SIZE);
    m.chunk_size = 1;
    f.manifest = serde_json::to_vec(&m).unwrap();
    f.envelope = Envelope::sign(&f.publisher, Context::Manifest, &f.manifest);
    assert_eq!(
        run(&f, &t, install()),
        Err(VerifyError::Manifest(ManifestError::ChunkSize(1)))
    );
}

#[test]
fn step4_identity_must_match_the_request() {
    let f0 = fixture();
    let t = state(&f0, |_| {});
    let other: Uuid = BOB.parse().unwrap();
    type Edit = fn(&mut ExpectedRelease, Uuid);
    let edits: [(&str, Edit); 4] = [
        ("package_id", |e, o| e.package_id = o),
        ("version_id", |e, o| e.version_id = o),
        ("platform", |e, _| e.platform = Platform::LinuxX86_64),
        ("sequence", |e, _| e.sequence += 1),
    ];
    for (field, edit) in edits {
        let mut f = fixture();
        edit(&mut f.expected, other);
        assert_eq!(
            run(&f, &t, install()),
            Err(VerifyError::Mismatch { field }),
            "{field}"
        );
    }
}

// ---- step 5: rollback -------------------------------------------------------

#[test]
fn step5_rollback_refused_unless_explicit() {
    let f = fixture();
    let t = state(&f, |_| {});
    let seq = f.expected.sequence;
    let r = verify_manifest(
        &t,
        &f.envelope,
        &f.manifest,
        &f.expected,
        Some(seq + 1),
        install(),
    );
    assert_eq!(
        r,
        Err(VerifyError::Rollback {
            installed: seq + 1,
            found: seq
        })
    );
    // Same sequence (repair / reinstall) is fine.
    verify_manifest(
        &t,
        &f.envelope,
        &f.manifest,
        &f.expected,
        Some(seq),
        install(),
    )
    .unwrap();
    // The explicit "install older version" action.
    verify_manifest(
        &t,
        &f.envelope,
        &f.manifest,
        &f.expected,
        Some(seq + 1),
        VerifyMode::Install {
            now: ts(NOW),
            allow_older: true,
        },
    )
    .unwrap();
}

// ---- step 6: paths ----------------------------------------------------------

#[test]
fn step6_path_traversal_manifest_refused() {
    let mut f = fixture();
    let t = state(&f, |_| {});
    let m = build(&[("../../.bashrc", 10)], PACK_SIZE);
    f.manifest = serde_json::to_vec(&m).unwrap();
    f.envelope = Envelope::sign(&f.publisher, Context::Manifest, &f.manifest);
    assert!(matches!(
        run(&f, &t, install()),
        Err(VerifyError::Manifest(ManifestError::Path(_)))
    ));
}

// ---- re-sign after revocation (end to end in core) ---------------------------

#[test]
fn resign_after_revocation() {
    let f = fixture();
    let new_key = key(3);
    // v2 revokes the old key and trusts the new one.
    let mut b = bundle(
        &f.root,
        2,
        vec![publisher(&f.publisher, ALICE), publisher(&new_key, ALICE)],
    );
    b.revoked.push(Revocation {
        key_id: f.publisher.public_key().key_id(),
        revoked_at: ts("2026-10-01T12:00:00Z"),
        reason: "rotation".into(),
    });
    let (bytes, sig) = signed(&f.root, &b);
    let t = verify_bundle(
        &bytes,
        &sig,
        &RootPin::new(f.root.public_key()),
        Some(1),
        server(),
    )
    .unwrap()
    .state;
    assert!(matches!(
        run(&f, &t, VerifyMode::Launch),
        Err(VerifyError::RevokedKey(_))
    ));
    // The manifest bytes do not change; only the envelope is swapped.
    let resigned = Envelope::sign(&new_key, Context::Manifest, &f.manifest);
    verify_manifest(
        &t,
        &resigned,
        &f.manifest,
        &f.expected,
        None,
        VerifyMode::Launch,
    )
    .unwrap();
}

// ---- compat profiles ---------------------------------------------------------

mod compat_profiles {
    use super::*;
    use crate::compat::tests::{PACKAGE, linux};

    fn signed_profile(
        f: &Fixture,
        edit: impl FnOnce(&mut serde_json::Value),
    ) -> (Vec<u8>, Envelope) {
        let mut v = linux();
        edit(&mut v);
        let bytes = serde_json::to_vec(&v).unwrap();
        let env = Envelope::sign(&f.publisher, Context::Compat, &bytes);
        (bytes, env)
    }

    fn expected() -> ExpectedCompat {
        ExpectedCompat {
            server_id: server(),
            package_id: PACKAGE.parse().unwrap(),
            target: Target::Linux,
        }
    }

    fn server_mode(caller: &str) -> VerifyMode {
        VerifyMode::Server {
            now: ts(NOW),
            caller: caller.parse().unwrap(),
        }
    }

    #[test]
    fn valid_profile_verifies() {
        let f = fixture();
        let t = state(&f, |_| {});
        let (bytes, env) = signed_profile(&f, |_| {});
        let v = verify_compat_profile(&t, &env, &bytes, &expected(), Some(2), VerifyMode::Launch)
            .unwrap();
        assert!(v.newer);
        assert_eq!(v.profile.revision, 3);
        verify_compat_profile(&t, &env, &bytes, &expected(), Some(2), server_mode(ALICE)).unwrap();
    }

    #[test]
    fn manifest_signature_is_not_a_compat_signature() {
        let f = fixture();
        let t = state(&f, |_| {});
        let (bytes, _) = signed_profile(&f, |_| {});
        let mut env = Envelope::sign(&f.publisher, Context::Manifest, &bytes);
        assert_eq!(
            verify_compat_profile(&t, &env, &bytes, &expected(), None, VerifyMode::Launch),
            Err(VerifyError::WrongContext(Context::Manifest))
        );
        env.context = Context::Compat;
        assert_eq!(
            verify_compat_profile(&t, &env, &bytes, &expected(), None, VerifyMode::Launch),
            Err(VerifyError::Signature(SignatureError::Invalid))
        );
    }

    #[test]
    fn revoked_key_and_holder_rules_apply() {
        let f = fixture();
        let t = state(&f, |b| {
            b.revoked.push(Revocation {
                key_id: f.publisher.public_key().key_id(),
                revoked_at: ts("2026-10-01T00:00:00Z"),
                reason: String::new(),
            })
        });
        let (bytes, env) = signed_profile(&f, |_| {});
        assert!(matches!(
            verify_compat_profile(&t, &env, &bytes, &expected(), None, VerifyMode::Launch),
            Err(VerifyError::RevokedKey(_))
        ));
        let t = state(&f, |_| {});
        assert!(matches!(
            verify_compat_profile(&t, &env, &bytes, &expected(), None, server_mode(BOB)),
            Err(VerifyError::NotKeyHolder(_))
        ));
    }

    #[test]
    fn identity_must_match() {
        let f = fixture();
        let t = state(&f, |_| {});
        let (bytes, env) = signed_profile(&f, |_| {});
        let mut e = expected();
        e.target = Target::Macos;
        assert_eq!(
            verify_compat_profile(&t, &env, &bytes, &e, None, VerifyMode::Launch),
            Err(VerifyError::CompatMismatch { field: "target" })
        );
        let mut e = expected();
        e.package_id = BOB.parse().unwrap();
        assert_eq!(
            verify_compat_profile(&t, &env, &bytes, &e, None, VerifyMode::Launch),
            Err(VerifyError::CompatMismatch {
                field: "package_id"
            })
        );
    }

    #[test]
    fn revision_rollback() {
        let f = fixture();
        let t = state(&f, |_| {});
        let (bytes, env) = signed_profile(&f, |_| {});
        // Lower than the one the launcher has: refused.
        assert_eq!(
            verify_compat_profile(&t, &env, &bytes, &expected(), Some(4), VerifyMode::Launch),
            Err(VerifyError::CompatRollback { last: 4, found: 3 })
        );
        // Same revision: a refresh for the launcher, a conflict for the server.
        let v = verify_compat_profile(&t, &env, &bytes, &expected(), Some(3), VerifyMode::Launch)
            .unwrap();
        assert!(!v.newer);
        assert_eq!(
            verify_compat_profile(&t, &env, &bytes, &expected(), Some(3), server_mode(ALICE)),
            Err(VerifyError::CompatRollback { last: 3, found: 3 })
        );
    }

    #[test]
    fn invalid_profile_even_if_signed() {
        let f = fixture();
        let t = state(&f, |_| {});
        let (bytes, env) = signed_profile(&f, |v| {
            v["runner"]["env"] = serde_json::json!({ "LD_PRELOAD": "/tmp/x.so" })
        });
        assert!(matches!(
            verify_compat_profile(&t, &env, &bytes, &expected(), None, VerifyMode::Launch),
            Err(VerifyError::Compat(_))
        ));
    }
}
