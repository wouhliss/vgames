//! Manifest verification (01-security §3.4), in one function shared by the
//! API (finalize, re-sign) and the launcher (install, update, pre-launch).
//!
//! Order, any failure aborts and nothing is used:
//! 1. The trust state comes from a verified bundle ([`crate::trust::verify_bundle`]);
//!    its `server_id` is the one expected, and for installs it is not expired.
//! 2. The envelope's `key_id` is a publisher in the bundle and is not revoked.
//!    Server mode also requires the key to be valid now and held by the caller.
//! 3. `BLAKE3(manifest_bytes) == payload_blake3`, strict Ed25519 under `vgames/manifest/v1`.
//! 4. The manifest parses and validates, and its identity equals the release requested.
//! 5. `sequence ≥ installed sequence` unless the user explicitly chose an older version.
//! 6. Every path passes the path rules (part of step 4's validation).
//!
//! [`verify_compat_profile`] applies the same key rules to `vgames.compat/1` profiles.

use uuid::Uuid;

use crate::codec::{Digest, Timestamp};
use crate::compat::{self, CompatError, CompatProfile, Target};
use crate::manifest::{self, Manifest, ManifestError, Platform};
use crate::sign::{Context, Envelope, KeyId, SignatureError};
use crate::trust::{KeyStatus, PublisherKey, TrustState};

/// The release the caller asked for (from the release descriptor and the
/// pinned server profile, or from the server's own version row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedRelease {
    pub server_id: Uuid,
    pub package_id: Uuid,
    pub version_id: Uuid,
    pub platform: Platform,
    pub sequence: u64,
}

/// Who is verifying, and why. The rules differ only where 01 §3.2 says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyMode {
    /// Launcher installing, updating or repairing. Refused while the trust
    /// bundle is expired. `allow_older` is set only by the explicit "install an
    /// older version" action (or when the installed version was yanked).
    Install { now: Timestamp, allow_older: bool },
    /// Launcher pre-launch check of an installed package: an expired bundle or
    /// an expired key does not block launching; revocation does.
    Launch,
    /// Server accepting a manifest (finalize or re-sign): the key must be valid
    /// at `now` and held by `caller`.
    Server { now: Timestamp, caller: Uuid },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    #[error("the trust bundle is for another server")]
    TrustServerMismatch,
    #[error("the trust bundle has expired; installs and updates wait for a fresh one")]
    TrustExpired,
    #[error("signature envelope is for {0}, not a manifest")]
    WrongContext(Context),
    #[error("signing key {0} is not in the trust bundle")]
    UnknownKey(KeyId),
    #[error("signing key {0} has been revoked")]
    RevokedKey(KeyId),
    #[error("signing key {0} is outside its validity window")]
    KeyNotValidNow(KeyId),
    #[error("signing key {0} is not held by the uploading user")]
    NotKeyHolder(KeyId),
    #[error("signature: {0}")]
    Signature(#[from] SignatureError),
    #[error("manifest: {0}")]
    Manifest(#[from] ManifestError),
    #[error("manifest {field} does not match the requested release")]
    Mismatch { field: &'static str },
    #[error("manifest sequence {found} is lower than the installed {installed} (rollback)")]
    Rollback { installed: u64, found: u64 },
    #[error("compat profile: {0}")]
    Compat(#[from] CompatError),
    #[error("compat profile {field} does not match the package")]
    CompatMismatch { field: &'static str },
    #[error("compat profile revision {found} is not newer than {last} (rollback)")]
    CompatRollback { last: u64, found: u64 },
}

/// A manifest that passed every step, with what the caller needs to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedManifest {
    pub manifest: Manifest,
    /// BLAKE3 of the exact bytes.
    pub digest: Digest,
    pub key_id: KeyId,
    pub holder_user_id: Uuid,
}

/// Steps 1–2 of 01 §3.4 for any publisher-signed document (manifests and
/// compat profiles share the key rules).
pub(crate) fn check_signing_key<'a>(
    trust: &'a TrustState,
    envelope: &Envelope,
    server_id: Uuid,
    context: Context,
    mode: VerifyMode,
) -> Result<&'a PublisherKey, VerifyError> {
    if trust.server_id() != server_id {
        return Err(VerifyError::TrustServerMismatch);
    }
    if let VerifyMode::Install { now, .. } = mode
        && trust.is_expired(now)
    {
        return Err(VerifyError::TrustExpired);
    }
    if envelope.context != context {
        return Err(VerifyError::WrongContext(envelope.context));
    }
    let key = match trust.key_status(&envelope.key_id) {
        KeyStatus::Trusted(p) => p,
        KeyStatus::Revoked => return Err(VerifyError::RevokedKey(envelope.key_id)),
        KeyStatus::Unknown => return Err(VerifyError::UnknownKey(envelope.key_id)),
    };
    if let VerifyMode::Server { now, caller } = mode {
        if now < key.not_before || now > key.not_after {
            return Err(VerifyError::KeyNotValidNow(key.key_id));
        }
        if key.holder_user_id != caller {
            return Err(VerifyError::NotKeyHolder(key.key_id));
        }
    }
    Ok(key)
}

/// Verifies a signed manifest end to end (01-security §3.4 steps 1–6).
///
/// `installed_sequence` is the sequence of the version currently installed
/// (launcher updates), or `None` (fresh install, pre-launch, server).
pub fn verify_manifest(
    trust: &TrustState,
    envelope: &Envelope,
    manifest_bytes: &[u8],
    expected: &ExpectedRelease,
    installed_sequence: Option<u64>,
    mode: VerifyMode,
) -> Result<VerifiedManifest, VerifyError> {
    // Steps 1–2.
    let key = check_signing_key(trust, envelope, expected.server_id, Context::Manifest, mode)?;
    // Step 3, over the exact bytes; nothing is parsed before it passes.
    let digest = Digest::of(manifest_bytes);
    envelope.verify_digest(&key.public_key, Context::Manifest, &digest)?;
    // Steps 4 and 6.
    let manifest = manifest::parse_and_validate(manifest_bytes)?;
    let checks: [(&'static str, bool); 5] = [
        ("server_id", manifest.server_id == expected.server_id),
        ("package_id", manifest.package_id == expected.package_id),
        ("version_id", manifest.version_id == expected.version_id),
        ("platform", manifest.platform == expected.platform),
        ("sequence", manifest.sequence == expected.sequence),
    ];
    if let Some((field, _)) = checks.iter().find(|(_, ok)| !ok) {
        return Err(VerifyError::Mismatch { field });
    }
    // Step 5.
    let allow_older = matches!(
        mode,
        VerifyMode::Install {
            allow_older: true,
            ..
        }
    );
    if let Some(installed) = installed_sequence
        && manifest.sequence < installed
        && !allow_older
    {
        return Err(VerifyError::Rollback {
            installed,
            found: manifest.sequence,
        });
    }
    Ok(VerifiedManifest {
        manifest,
        digest,
        key_id: key.key_id,
        holder_user_id: key.holder_user_id,
    })
}

/// What a compat profile must be for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedCompat {
    pub server_id: Uuid,
    pub package_id: Uuid,
    pub target: Target,
}

/// A compat profile that passed [`verify_compat_profile`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCompat {
    pub profile: CompatProfile,
    pub digest: Digest,
    pub key_id: KeyId,
    pub holder_user_id: Uuid,
    /// True when `revision` is greater than `last_revision` (or none was known).
    pub newer: bool,
}

/// Verifies a signed `vgames.compat/1` profile with the same key rules as
/// manifests (context `vgames/compat/v1`). Launchers refuse a revision lower
/// than the one they have (`last_revision`); the server (`VerifyMode::Server`)
/// requires a strictly greater one.
pub fn verify_compat_profile(
    trust: &TrustState,
    envelope: &Envelope,
    profile_bytes: &[u8],
    expected: &ExpectedCompat,
    last_revision: Option<u64>,
    mode: VerifyMode,
) -> Result<VerifiedCompat, VerifyError> {
    let key = check_signing_key(trust, envelope, expected.server_id, Context::Compat, mode)?;
    let digest = Digest::of(profile_bytes);
    envelope.verify_digest(&key.public_key, Context::Compat, &digest)?;
    let profile = compat::parse_and_validate(profile_bytes)?;
    let checks: [(&'static str, bool); 3] = [
        ("server_id", profile.server_id == expected.server_id),
        ("package_id", profile.package_id == expected.package_id),
        ("target", profile.target == expected.target),
    ];
    if let Some((field, _)) = checks.iter().find(|(_, ok)| !ok) {
        return Err(VerifyError::CompatMismatch { field });
    }
    let newer = last_revision.is_none_or(|last| profile.revision > last);
    if let Some(last) = last_revision {
        let strict = matches!(mode, VerifyMode::Server { .. });
        if profile.revision < last || (strict && profile.revision == last) {
            return Err(VerifyError::CompatRollback {
                last,
                found: profile.revision,
            });
        }
    }
    Ok(VerifiedCompat {
        profile,
        digest,
        key_id: key.key_id,
        holder_user_id: key.holder_user_id,
        newer,
    })
}

#[cfg(test)]
mod tests;
