use vgames_core::Context;
use vgames_proto::versions::{CompatStatus, SignatureEnvelope};
use vgames_transfer::testkit::package::{
    PACKAGE_ID, SERVER_ID, publisher_key, second_publisher_key, trust_state, trust_state_with,
};

use super::*;

struct Fake {
    list: Mutex<Vec<SignedCompatProfile>>,
    trust: Mutex<Option<Arc<TrustState>>>,
}

impl ProfileSource for Fake {
    async fn fetch(&self, _: Uuid, _: Uuid) -> Result<CompatProfileList, ProfileError> {
        Ok(CompatProfileList {
            items: self.list.lock().unwrap().clone(),
        })
    }
    async fn trust(&self, _: Uuid) -> Option<Arc<TrustState>> {
        self.trust.lock().unwrap().clone()
    }
}

const PACKAGE: PackageRef = PackageRef {
    server_id: SERVER_ID,
    package_id: PACKAGE_ID,
};

fn document(target: &str, revision: u64, env: serde_json::Value) -> Vec<u8> {
    let kind = if target == "linux" { "proton" } else { "wine" };
    serde_json::to_vec(&serde_json::json!({
        "format": "vgames.compat/1",
        "server_id": SERVER_ID, "package_id": PACKAGE_ID,
        "target": target, "revision": revision,
        "created_at": "2026-10-01T10:00:00Z",
        "applies_to": {"platform": "windows-x86_64", "min_sequence": 2, "max_sequence": 4},
        "status": "playable",
        "runner": {"kind": kind, "env": env}
    }))
    .unwrap()
}

fn signed_with(
    key: &vgames_core::SecretKey,
    target: &str,
    revision: u64,
    env: serde_json::Value,
) -> SignedCompatProfile {
    let bytes = document(target, revision, env);
    let envelope = vgames_core::Envelope::sign(key, Context::Compat, &bytes);
    let signature: SignatureEnvelope =
        serde_json::from_slice(&serde_json::to_vec(&envelope).unwrap()).unwrap();
    SignedCompatProfile {
        target: if target == "linux" {
            CompatTarget::Linux
        } else {
            CompatTarget::Macos
        },
        revision: i64::try_from(revision).unwrap(),
        status: CompatStatus::Playable,
        document: base64::engine::general_purpose::STANDARD.encode(&bytes),
        signature,
        created_at: time::OffsetDateTime::UNIX_EPOCH,
    }
}

fn signed(target: &str, revision: u64) -> SignedCompatProfile {
    signed_with(
        &publisher_key(),
        target,
        revision,
        serde_json::json!({"DXVK_HUD": "fps"}),
    )
}

fn rig(db: Db) -> (Arc<Fake>, Profiles<Fake>) {
    let fake = Arc::new(Fake {
        list: Mutex::new(Vec::new()),
        trust: Mutex::new(Some(Arc::new(trust_state()))),
    });
    (Arc::clone(&fake), Profiles::new(fake, db))
}

#[tokio::test]
async fn a_verified_profile_is_stored_and_a_newer_revision_replaces_it() {
    let (fake, profiles) = rig(Db::open_in_memory().unwrap());
    *fake.list.lock().unwrap() = vec![signed("linux", 3)];
    profiles.refresh(PACKAGE).await.unwrap();
    assert_eq!(
        profiles.state(PACKAGE, Target::Linux).await,
        ProfileState::Verified { revision: 3 }
    );
    assert_eq!(
        profiles.state(PACKAGE, Target::Macos).await,
        ProfileState::Missing
    );
    let p = profiles
        .current(PACKAGE, Target::Linux)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.revision, 3);
    assert_eq!(
        p.runner.env.get("DXVK_HUD").map(String::as_str),
        Some("fps")
    );

    *fake.list.lock().unwrap() = vec![signed("linux", 5)];
    profiles.refresh(PACKAGE).await.unwrap();
    assert_eq!(
        profiles
            .current(PACKAGE, Target::Linux)
            .await
            .unwrap()
            .unwrap()
            .revision,
        5
    );
}

#[tokio::test]
async fn an_older_revision_is_refused_and_the_stored_one_stays() {
    let (fake, profiles) = rig(Db::open_in_memory().unwrap());
    *fake.list.lock().unwrap() = vec![signed("linux", 5)];
    profiles.refresh(PACKAGE).await.unwrap();
    *fake.list.lock().unwrap() = vec![signed("linux", 4)];
    profiles.refresh(PACKAGE).await.unwrap();
    assert_eq!(
        profiles.state(PACKAGE, Target::Linux).await,
        ProfileState::Ignored {
            reason: ProfileIgnored::Rollback,
            revision: Some(5)
        }
    );
    assert_eq!(
        profiles
            .current(PACKAGE, Target::Linux)
            .await
            .unwrap()
            .unwrap()
            .revision,
        5
    );
}

#[tokio::test]
async fn a_profile_signed_by_a_revoked_key_is_ignored_with_that_status() {
    let (fake, profiles) = rig(Db::open_in_memory().unwrap());
    *fake.trust.lock().unwrap() = Some(Arc::new(trust_state_with(2, true)));
    *fake.list.lock().unwrap() = vec![signed("linux", 1)];
    profiles.refresh(PACKAGE).await.unwrap();
    assert_eq!(
        profiles.state(PACKAGE, Target::Linux).await,
        ProfileState::Ignored {
            reason: ProfileIgnored::RevokedKey,
            revision: None
        }
    );
    assert!(
        profiles
            .current(PACKAGE, Target::Linux)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_stored_profile_stops_applying_once_its_key_is_revoked() {
    let (fake, profiles) = rig(Db::open_in_memory().unwrap());
    *fake.list.lock().unwrap() = vec![signed("linux", 1)];
    profiles.refresh(PACKAGE).await.unwrap();
    // The highest bundle version wins: the next one revokes the key.
    *fake.trust.lock().unwrap() = Some(Arc::new(trust_state_with(2, true)));
    assert!(
        profiles
            .current(PACKAGE, Target::Linux)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        profiles.state(PACKAGE, Target::Linux).await,
        ProfileState::Ignored {
            reason: ProfileIgnored::RevokedKey,
            revision: None
        }
    );
}

#[tokio::test]
async fn unknown_keys_and_launcher_owned_env_keys_are_refused() {
    let (fake, profiles) = rig(Db::open_in_memory().unwrap());
    *fake.list.lock().unwrap() = vec![signed_with(
        &second_publisher_key(),
        "linux",
        1,
        serde_json::json!({}),
    )];
    profiles.refresh(PACKAGE).await.unwrap();
    assert_eq!(
        profiles.state(PACKAGE, Target::Linux).await,
        ProfileState::Ignored {
            reason: ProfileIgnored::UnknownKey,
            revision: None
        }
    );
    for key in [
        "WINEPREFIX",
        "PROTONPATH",
        "STEAM_COMPAT_DATA_PATH",
        "UMU_ID",
        "WINEDLLOVERRIDES",
    ] {
        *fake.list.lock().unwrap() = vec![signed_with(
            &publisher_key(),
            "linux",
            1,
            serde_json::json!({ key: "/tmp/x" }),
        )];
        profiles.refresh(PACKAGE).await.unwrap();
        assert_eq!(
            profiles.state(PACKAGE, Target::Linux).await,
            ProfileState::Ignored {
                reason: ProfileIgnored::Invalid,
                revision: None
            },
            "{key}"
        );
    }
    assert!(
        profiles
            .current(PACKAGE, Target::Linux)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn without_trust_nothing_is_accepted() {
    let (fake, profiles) = rig(Db::open_in_memory().unwrap());
    *fake.trust.lock().unwrap() = None;
    *fake.list.lock().unwrap() = vec![signed("macos", 1)];
    profiles.refresh(PACKAGE).await.unwrap();
    assert_eq!(
        profiles.state(PACKAGE, Target::Macos).await,
        ProfileState::Ignored {
            reason: ProfileIgnored::NoTrust,
            revision: None
        }
    );
}

#[tokio::test]
async fn applies_to_bounds_hold() {
    let (fake, profiles) = rig(Db::open_in_memory().unwrap());
    *fake.list.lock().unwrap() = vec![signed("linux", 1)];
    profiles.refresh(PACKAGE).await.unwrap();
    for (platform, sequence, applies) in [
        (Platform::WindowsX86_64, 1, false),
        (Platform::WindowsX86_64, 2, true),
        (Platform::WindowsX86_64, 4, true),
        (Platform::WindowsX86_64, 5, false),
        (Platform::WindowsAarch64, 3, false),
    ] {
        assert_eq!(
            profiles
                .for_release(PACKAGE, Target::Linux, platform, sequence)
                .await
                .unwrap()
                .is_some(),
            applies,
            "{platform:?} {sequence}"
        );
    }
}

#[tokio::test]
async fn the_stored_profile_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("launcher.db");
    {
        let (fake, profiles) = rig(Db::open(&path).unwrap());
        *fake.list.lock().unwrap() = vec![signed("linux", 2), signed("macos", 7)];
        profiles.refresh(PACKAGE).await.unwrap();
    }
    let (_fake, profiles) = rig(Db::open(&path).unwrap());
    assert_eq!(
        profiles
            .current(PACKAGE, Target::Linux)
            .await
            .unwrap()
            .unwrap()
            .revision,
        2
    );
    assert_eq!(
        profiles
            .current(PACKAGE, Target::Macos)
            .await
            .unwrap()
            .unwrap()
            .revision,
        7
    );
    assert_eq!(
        profiles.state(PACKAGE, Target::Macos).await,
        ProfileState::Verified { revision: 7 }
    );
}

#[tokio::test]
async fn a_tampered_stored_document_is_not_used() {
    let (fake, profiles) = rig(Db::open_in_memory().unwrap());
    *fake.list.lock().unwrap() = vec![signed("linux", 1)];
    profiles.refresh(PACKAGE).await.unwrap();
    let tampered = document("linux", 1, serde_json::json!({"DXVK_HUD": "full"}));
    profiles
        .db
        .call(move |conn| {
            conn.execute("UPDATE compat_profiles SET document = ?1", [tampered])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        profiles
            .current(PACKAGE, Target::Linux)
            .await
            .unwrap()
            .is_none()
    );
}
