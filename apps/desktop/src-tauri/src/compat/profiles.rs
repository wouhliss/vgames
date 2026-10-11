//! Signed compat profiles in the launcher (09-compatibility §4, GAME-04).
//!
//! `GET /v1/packages/{package_id}/compat` (through `ApiClient`) returns the
//! latest signed profile per target. Each one is verified with
//! `vgames_core::verify::verify_compat_profile` under the server's trust state
//! (the highest bundle version wins), and the highest revision per (server,
//! package, target) is stored with its exact bytes and envelope. Stored profiles
//! are verified again when read, so a key revoked later disables them.
//!
//! Profiles are fetched with package details, before an install and before a
//! compat launch, never on a timer. A profile that fails verification is ignored
//! and its [`ProfileState::Ignored`] reason is kept for the UI (GAME-07).

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use reqwest::Method;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use specta::Type;
use uuid::Uuid;
use vgames_core::compat::{CompatProfile, Target};
use vgames_core::manifest::Platform;
use vgames_core::trust::TrustState;
use vgames_core::verify::{ExpectedCompat, VerifyError, VerifyMode, verify_compat_profile};
use vgames_proto::versions::{CompatProfileList, CompatTarget, SignedCompatProfile};

use crate::db::{Db, DbError, now_unix};
use crate::events::PackageRef;

/// Where a package's profile for one target stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProfileState {
    /// No profile was published (defaults apply).
    Missing,
    /// A verified profile is in use.
    Verified { revision: u64 },
    /// The server sent a profile the launcher refused; a stored one, if any, stays.
    Ignored {
        reason: ProfileIgnored,
        revision: Option<u64>,
    },
}

/// Why a profile was ignored (shown in package details and Settings).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ProfileIgnored {
    /// No verified trust bundle for this server yet.
    NoTrust,
    /// Signed by a key the bundle does not list.
    UnknownKey,
    /// Signed by a revoked key.
    RevokedKey,
    /// The signature, the document or its ids do not check out.
    Invalid,
    /// Older than the revision this launcher already has.
    Rollback,
}

impl ProfileIgnored {
    fn from_error(error: &VerifyError) -> Self {
        match error {
            VerifyError::UnknownKey(_) => Self::UnknownKey,
            VerifyError::RevokedKey(_) => Self::RevokedKey,
            VerifyError::CompatRollback { .. } => Self::Rollback,
            _ => Self::Invalid,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("compat profiles could not be fetched: {0}")]
    Unavailable(String),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// Where profiles and trust come from (the servers in the app, fakes in tests).
pub trait ProfileSource: Send + Sync + 'static {
    fn fetch(
        &self,
        server_id: Uuid,
        package_id: Uuid,
    ) -> impl Future<Output = Result<CompatProfileList, ProfileError>> + Send;

    fn trust(&self, server_id: Uuid) -> impl Future<Output = Option<Arc<TrustState>>> + Send;
}

impl ProfileSource for crate::servers::Servers {
    async fn fetch(
        &self,
        server_id: Uuid,
        package_id: Uuid,
    ) -> Result<CompatProfileList, ProfileError> {
        let api = self
            .api(server_id)
            .await
            .map_err(|e| ProfileError::Unavailable(e.to_string()))?;
        api.authed(
            Method::GET,
            &format!("v1/packages/{package_id}/compat"),
            None::<&()>,
        )
        .await
        .map_err(|e| ProfileError::Unavailable(e.to_string()))
    }

    async fn trust(&self, server_id: Uuid) -> Option<Arc<TrustState>> {
        self.trust_state(server_id).await.ok().flatten()
    }
}

type Key = (PackageRef, Target);

pub struct Profiles<S> {
    source: Arc<S>,
    db: Db,
    states: Mutex<HashMap<Key, ProfileState>>,
}

fn target_of(t: CompatTarget) -> Target {
    match t {
        CompatTarget::Linux => Target::Linux,
        CompatTarget::Macos => Target::Macos,
    }
}

impl<S: ProfileSource> Profiles<S> {
    pub fn new(source: Arc<S>, db: Db) -> Self {
        Self {
            source,
            db,
            states: Mutex::new(HashMap::new()),
        }
    }

    fn set_state(&self, key: Key, state: ProfileState) {
        self.states
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, state);
    }

    /// Fetches the package's profiles, verifies each and stores the newer ones.
    pub async fn refresh(&self, package: PackageRef) -> Result<(), ProfileError> {
        let list = self
            .source
            .fetch(package.server_id, package.package_id)
            .await?;
        let trust = self.source.trust(package.server_id).await;
        let mut seen = Vec::new();
        for signed in list.items {
            let target = target_of(signed.target);
            if seen.contains(&target) {
                continue; // one per target; the server sends the latest first
            }
            seen.push(target);
            let key = (package, target);
            let stored = stored_revision(&self.db, package, target).await?;
            let Some(trust) = trust.as_deref() else {
                self.set_state(
                    key,
                    ProfileState::Ignored {
                        reason: ProfileIgnored::NoTrust,
                        revision: stored,
                    },
                );
                continue;
            };
            match accept(trust, package, &signed, stored) {
                Ok((profile_bytes, envelope_bytes, revision)) => {
                    if stored.is_none_or(|s| revision > s) {
                        store(
                            &self.db,
                            package,
                            target,
                            revision,
                            profile_bytes,
                            envelope_bytes,
                        )
                        .await?;
                    }
                    self.set_state(key, ProfileState::Verified { revision });
                }
                Err(reason) => {
                    tracing::warn!(
                        server_id = %package.server_id,
                        package_id = %package.package_id,
                        target = target.as_str(),
                        ?reason,
                        "compat profile ignored"
                    );
                    self.set_state(
                        key,
                        ProfileState::Ignored {
                            reason,
                            revision: stored,
                        },
                    );
                }
            }
        }
        for target in [Target::Linux, Target::Macos] {
            if !seen.contains(&target)
                && stored_revision(&self.db, package, target).await?.is_none()
            {
                self.set_state((package, target), ProfileState::Missing);
            }
        }
        Ok(())
    }

    /// The stored profile for `target`, verified again under the current trust.
    pub async fn current(
        &self,
        package: PackageRef,
        target: Target,
    ) -> Result<Option<CompatProfile>, ProfileError> {
        let Some((document, envelope)) = stored(&self.db, package, target).await? else {
            return Ok(None);
        };
        let Some(trust) = self.source.trust(package.server_id).await else {
            self.set_state(
                (package, target),
                ProfileState::Ignored {
                    reason: ProfileIgnored::NoTrust,
                    revision: None,
                },
            );
            return Ok(None);
        };
        let expected = ExpectedCompat {
            server_id: package.server_id,
            package_id: package.package_id,
            target,
        };
        let verified = vgames_core::Envelope::parse(&envelope)
            .map_err(|_| ProfileIgnored::Invalid)
            .and_then(|env| {
                verify_compat_profile(&trust, &env, &document, &expected, None, VerifyMode::Launch)
                    .map_err(|e| ProfileIgnored::from_error(&e))
            });
        match verified {
            Ok(v) => Ok(Some(v.profile)),
            Err(reason) => {
                self.set_state(
                    (package, target),
                    ProfileState::Ignored {
                        reason,
                        revision: None,
                    },
                );
                Ok(None)
            }
        }
    }

    /// The profile that applies to a release (`CompatProfile::applies_to`), if any.
    pub async fn for_release(
        &self,
        package: PackageRef,
        target: Target,
        platform: Platform,
        sequence: u64,
    ) -> Result<Option<CompatProfile>, ProfileError> {
        Ok(self
            .current(package, target)
            .await?
            .filter(|p| p.applies_to(platform, sequence)))
    }

    /// What the UI shows for one target (from the last refresh or read).
    pub async fn state(&self, package: PackageRef, target: Target) -> ProfileState {
        if let Some(state) = self
            .states
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(package, target))
            .cloned()
        {
            return state;
        }
        match stored_revision(&self.db, package, target).await {
            Ok(Some(revision)) => ProfileState::Verified { revision },
            _ => ProfileState::Missing,
        }
    }
}

/// Verifies one signed profile; the exact document bytes, envelope bytes and revision.
fn accept(
    trust: &TrustState,
    package: PackageRef,
    signed: &SignedCompatProfile,
    stored: Option<u64>,
) -> Result<(Vec<u8>, Vec<u8>, u64), ProfileIgnored> {
    let document = base64::engine::general_purpose::STANDARD
        .decode(&signed.document)
        .map_err(|_| ProfileIgnored::Invalid)?;
    let envelope_bytes =
        serde_json::to_vec(&signed.signature).map_err(|_| ProfileIgnored::Invalid)?;
    let envelope =
        vgames_core::Envelope::parse(&envelope_bytes).map_err(|_| ProfileIgnored::Invalid)?;
    let expected = ExpectedCompat {
        server_id: package.server_id,
        package_id: package.package_id,
        target: target_of(signed.target),
    };
    let verified = verify_compat_profile(
        trust,
        &envelope,
        &document,
        &expected,
        stored,
        VerifyMode::Launch,
    )
    .map_err(|e| ProfileIgnored::from_error(&e))?;
    Ok((document, envelope_bytes, verified.profile.revision))
}

fn ids(package: PackageRef, target: Target) -> (String, String, &'static str) {
    (
        package.server_id.to_string(),
        package.package_id.to_string(),
        target.as_str(),
    )
}

async fn stored_revision(
    db: &Db,
    package: PackageRef,
    target: Target,
) -> Result<Option<u64>, DbError> {
    let (server, pkg, target) = ids(package, target);
    db.call(move |conn| {
        let v: Option<i64> = conn
            .query_row(
                "SELECT revision FROM compat_profiles
                  WHERE server_id = ?1 AND package_id = ?2 AND target = ?3",
                params![server, pkg, target],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v.and_then(|v| u64::try_from(v).ok()))
    })
    .await
}

async fn stored(
    db: &Db,
    package: PackageRef,
    target: Target,
) -> Result<Option<(Vec<u8>, Vec<u8>)>, DbError> {
    let (server, pkg, target) = ids(package, target);
    db.call(move |conn| {
        Ok(conn
            .query_row(
                "SELECT document, envelope FROM compat_profiles
                  WHERE server_id = ?1 AND package_id = ?2 AND target = ?3",
                params![server, pkg, target],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    })
    .await
}

async fn store(
    db: &Db,
    package: PackageRef,
    target: Target,
    revision: u64,
    document: Vec<u8>,
    envelope: Vec<u8>,
) -> Result<(), DbError> {
    let (server, pkg, target) = ids(package, target);
    let revision = i64::try_from(revision).unwrap_or(i64::MAX);
    db.call(move |conn| {
        // Guarded: never replace a higher revision.
        conn.execute(
            "INSERT INTO compat_profiles (server_id, package_id, target, revision, document, envelope, stored_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (server_id, package_id, target) DO UPDATE SET
                 revision = excluded.revision, document = excluded.document,
                 envelope = excluded.envelope, stored_at = excluded.stored_at
             WHERE excluded.revision > compat_profiles.revision",
            params![server, pkg, target, revision, document, envelope, now_unix()],
        )?;
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests;
