//! `vgames.trust/1` root-signed trust bundles (01-security §3.2).
//!
//! A bundle lists the publisher keys a server's root trusts, the revoked keys,
//! and optionally the next root key. [`verify_bundle`] is the only way to turn
//! bundle bytes into a [`TrustState`]; the API (upload), the launcher (fetch
//! and startup) and the CLI (`trust verify`) all call it.
//!
//! Rules:
//! - The root signs the exact bundle bytes under context `vgames/trust/v1`.
//! - `version` never goes down: a bundle older than the last one seen is refused.
//! - `expires_at` (optional): once past, launchers keep launching installed
//!   packages but refuse new installs and updates ([`TrustState::is_expired`]).
//! - Publisher `not_before`/`not_after` only limit what the **server** accepts
//!   for new uploads; launchers never treat key expiry as distrust.
//! - Revocation is the only thing that makes launchers distrust a key.
//! - Root rotation: a bundle signed by the pinned root may announce `next_root`.
//!   A later bundle signed by that key is accepted and the pin moves to it
//!   ([`RootPin`]). There is no other rotation path.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::codec::{self, Timestamp};
use crate::manifest::{parse_strict_uuid, strict_uuid};
use crate::sign::{Context, KeyId, PublicKey, SecretKey, Signature};

pub const FORMAT: &str = "vgames.trust/1";
/// Largest accepted bundle document (the API's JSON body limit is 1 MiB with base64).
pub const MAX_BUNDLE_BYTES: usize = 512 * 1024;
pub const MAX_PUBLISHERS: usize = 1024;
pub const MAX_REVOKED: usize = 4096;
pub const MAX_LABEL_CHARS: usize = 128;
pub const MAX_REASON_CHARS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublisherKey {
    pub key_id: KeyId,
    pub public_key: PublicKey,
    #[serde(deserialize_with = "strict_uuid")]
    pub holder_user_id: Uuid,
    pub label: String,
    pub not_before: Timestamp,
    pub not_after: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revocation {
    pub key_id: KeyId,
    pub revoked_at: Timestamp,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NextRoot {
    pub public_key: PublicKey,
    pub key_id: KeyId,
}

/// The `vgames.trust/1` document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustBundle {
    pub format: String,
    #[serde(deserialize_with = "strict_uuid")]
    pub server_id: Uuid,
    pub version: u64,
    pub issued_at: Timestamp,
    pub expires_at: Option<Timestamp>,
    pub root_key_id: KeyId,
    pub publishers: Vec<PublisherKey>,
    pub revoked: Vec<Revocation>,
    pub next_root: Option<NextRoot>,
}

/// What a launcher persists per server: the pinned root, plus the next root
/// announced by the last verified bundle (if any).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootPin {
    pub root: PublicKey,
    #[serde(default)]
    pub next_root: Option<PublicKey>,
}

impl RootPin {
    /// A pin with no pending rotation (first connection / `fp=` link).
    pub fn new(root: PublicKey) -> Self {
        Self {
            root,
            next_root: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrustError {
    #[error("trust bundle is larger than {MAX_BUNDLE_BYTES} bytes")]
    TooLarge,
    #[error("trust bundle signature is not valid for the pinned root key")]
    Signature,
    #[error("trust bundle is not valid vgames.trust/1 JSON: {0}")]
    Json(String),
    #[error("unknown trust bundle format {0:?}")]
    Format(String),
    #[error("trust bundle root_key_id does not match the key that signed it")]
    RootKeyId,
    #[error("trust bundle is for server {found}, expected {expected}")]
    ServerId { expected: Uuid, found: Uuid },
    #[error("trust bundle version {found} is older than {last_seen} (rollback)")]
    Rollback { last_seen: u64, found: u64 },
    #[error("trust bundle version must be at least 1")]
    ZeroVersion,
    #[error("expires_at is not after issued_at")]
    Expiry,
    #[error("publishers[{index}]: {fault}")]
    Publisher { index: usize, fault: EntryFault },
    #[error("revoked[{index}]: {fault}")]
    Revoked { index: usize, fault: EntryFault },
    #[error("next_root: {0}")]
    NextRoot(EntryFault),
    #[error("too many publishers or revocations")]
    TooManyEntries,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EntryFault {
    #[error("key_id does not match the public key")]
    KeyId,
    #[error("key id listed twice")]
    Duplicate,
    #[error("the root key cannot be a publisher key")]
    RootAsPublisher,
    #[error("label must be 1 to {MAX_LABEL_CHARS} characters without control characters")]
    Label,
    #[error("reason must be at most {MAX_REASON_CHARS} characters without control characters")]
    Reason,
    #[error("not_before must be before not_after")]
    Window,
    #[error("next_root is the current root")]
    SameRoot,
}

/// A bundle that passed [`verify_bundle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedBundle {
    pub state: TrustState,
    /// The pin to persist from now on (it changes when a rotation completes or
    /// a new `next_root` is announced).
    pub pin: RootPin,
    /// True when the bundle was signed by the announced next root (rotation completed).
    pub rotated: bool,
    /// True when `version` is greater than `last_seen_version` (or none was seen).
    /// The server stores only newer bundles; launchers treat an equal version as a refresh.
    pub newer: bool,
}

/// The trusted view of one server's publisher keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustState {
    bundle: TrustBundle,
    publishers: HashMap<KeyId, usize>,
    revoked: HashSet<KeyId>,
}

/// What the trust state says about a signing key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStatus<'a> {
    Trusted(&'a PublisherKey),
    Revoked,
    Unknown,
}

impl TrustState {
    pub fn bundle(&self) -> &TrustBundle {
        &self.bundle
    }
    pub fn server_id(&self) -> Uuid {
        self.bundle.server_id
    }
    pub fn version(&self) -> u64 {
        self.bundle.version
    }
    pub fn expires_at(&self) -> Option<Timestamp> {
        self.bundle.expires_at
    }
    /// Past `expires_at`: installed packages still launch, installs and updates are refused.
    pub fn is_expired(&self, now: Timestamp) -> bool {
        self.bundle.expires_at.is_some_and(|e| now >= e)
    }
    pub fn is_revoked(&self, key_id: &KeyId) -> bool {
        self.revoked.contains(key_id)
    }
    /// A publisher key listed in the bundle, revoked or not.
    pub fn publisher(&self, key_id: &KeyId) -> Option<&PublisherKey> {
        self.publishers
            .get(key_id)
            .and_then(|&i| self.bundle.publishers.get(i))
    }
    /// Revocation wins over listing.
    pub fn key_status(&self, key_id: &KeyId) -> KeyStatus<'_> {
        if self.is_revoked(key_id) {
            return KeyStatus::Revoked;
        }
        match self.publisher(key_id) {
            Some(p) => KeyStatus::Trusted(p),
            None => KeyStatus::Unknown,
        }
    }
}

fn valid_text(s: &str, min: usize, max: usize) -> bool {
    let n = s.chars().count();
    (min..=max).contains(&n) && !s.chars().any(char::is_control)
}

impl TrustBundle {
    /// Structural rules that do not depend on the signer or the clock.
    fn check(&self) -> Result<(), TrustError> {
        if self.format != FORMAT {
            return Err(TrustError::Format(self.format.chars().take(64).collect()));
        }
        if self.version == 0 {
            return Err(TrustError::ZeroVersion);
        }
        if self.expires_at.is_some_and(|e| e <= self.issued_at) {
            return Err(TrustError::Expiry);
        }
        if self.publishers.len() > MAX_PUBLISHERS || self.revoked.len() > MAX_REVOKED {
            return Err(TrustError::TooManyEntries);
        }
        let mut ids = HashSet::new();
        for (index, p) in self.publishers.iter().enumerate() {
            let fault = |fault| TrustError::Publisher { index, fault };
            if p.public_key.key_id() != p.key_id {
                return Err(fault(EntryFault::KeyId));
            }
            if p.key_id == self.root_key_id {
                return Err(fault(EntryFault::RootAsPublisher));
            }
            if !ids.insert(p.key_id) {
                return Err(fault(EntryFault::Duplicate));
            }
            if !valid_text(&p.label, 1, MAX_LABEL_CHARS) {
                return Err(fault(EntryFault::Label));
            }
            if p.not_before >= p.not_after {
                return Err(fault(EntryFault::Window));
            }
        }
        let mut revoked = HashSet::new();
        for (index, r) in self.revoked.iter().enumerate() {
            let fault = |fault| TrustError::Revoked { index, fault };
            if !revoked.insert(r.key_id) {
                return Err(fault(EntryFault::Duplicate));
            }
            if !valid_text(&r.reason, 0, MAX_REASON_CHARS) {
                return Err(fault(EntryFault::Reason));
            }
        }
        if let Some(next) = &self.next_root {
            if next.public_key.key_id() != next.key_id {
                return Err(TrustError::NextRoot(EntryFault::KeyId));
            }
            if next.key_id == self.root_key_id {
                return Err(TrustError::NextRoot(EntryFault::SameRoot));
            }
        }
        Ok(())
    }

    /// Canonical JSON bytes of a bundle being built (CLI `trust build`). The
    /// signature covers whatever bytes are produced here; verification never
    /// re-serializes.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).unwrap_or_default()
    }

    /// Parses bundle bytes without verifying a signature (for inspection
    /// tools only: `vgames trust verify` still calls [`verify_bundle`]).
    pub fn parse_unverified(bytes: &[u8]) -> Result<Self, TrustError> {
        if bytes.len() > MAX_BUNDLE_BYTES {
            return Err(TrustError::TooLarge);
        }
        let b: TrustBundle =
            serde_json::from_slice(bytes).map_err(|e| TrustError::Json(e.to_string()))?;
        b.check()?;
        Ok(b)
    }
}

/// Signs exact bundle bytes with the root key (offline, CLI `trust sign`).
pub fn sign_bundle(root: &SecretKey, bundle_bytes: &[u8]) -> Signature {
    root.sign(Context::Trust, bundle_bytes)
}

/// Verifies a trust bundle (01-security §3.2) and builds the trust state.
///
/// - `pin`: the root pinned for this server (and any announced next root).
/// - `last_seen_version`: the highest bundle version accepted so far, if any.
/// - `server_id`: the server this bundle must belong to.
///
/// Expiry is not an error here: check [`TrustState::is_expired`] (installs) or
/// use `verify_manifest`, which does.
pub fn verify_bundle(
    bundle_bytes: &[u8],
    signature: &Signature,
    pin: &RootPin,
    last_seen_version: Option<u64>,
    server_id: Uuid,
) -> Result<VerifiedBundle, TrustError> {
    if bundle_bytes.len() > MAX_BUNDLE_BYTES {
        return Err(TrustError::TooLarge);
    }
    // Signature first, over the exact bytes: nothing is parsed before it verifies.
    let (signer, rotated) = if pin
        .root
        .verify(Context::Trust, bundle_bytes, signature)
        .is_ok()
    {
        (pin.root, false)
    } else {
        match &pin.next_root {
            Some(next) if next.verify(Context::Trust, bundle_bytes, signature).is_ok() => {
                (*next, true)
            }
            _ => return Err(TrustError::Signature),
        }
    };
    let bundle: TrustBundle =
        serde_json::from_slice(bundle_bytes).map_err(|e| TrustError::Json(e.to_string()))?;
    bundle.check()?;
    if bundle.root_key_id != signer.key_id() {
        return Err(TrustError::RootKeyId);
    }
    if bundle.server_id != server_id {
        return Err(TrustError::ServerId {
            expected: server_id,
            found: bundle.server_id,
        });
    }
    if let Some(last_seen) = last_seen_version
        && bundle.version < last_seen
    {
        return Err(TrustError::Rollback {
            last_seen,
            found: bundle.version,
        });
    }
    let newer = last_seen_version.is_none_or(|v| bundle.version > v);
    let pin = RootPin {
        root: signer,
        next_root: bundle.next_root.as_ref().map(|n| n.public_key),
    };
    let publishers = bundle
        .publishers
        .iter()
        .enumerate()
        .map(|(i, p)| (p.key_id, i))
        .collect();
    let revoked = bundle.revoked.iter().map(|r| r.key_id).collect();
    Ok(VerifiedBundle {
        state: TrustState {
            bundle,
            publishers,
            revoked,
        },
        pin,
        rotated,
        newer,
    })
}

/// The `GET /v1/trust/bundle` body: `{ "bundle": base64, "signature": base64 }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedBundle {
    pub bundle: String,
    pub signature: Signature,
}

impl SignedBundle {
    pub fn new(bundle_bytes: &[u8], signature: Signature) -> Self {
        Self {
            bundle: codec::encode_base64(bundle_bytes),
            signature,
        }
    }

    /// Decodes the bundle bytes (bounded) for [`verify_bundle`].
    pub fn bundle_bytes(&self) -> Result<Vec<u8>, TrustError> {
        if self.bundle.len() > MAX_BUNDLE_BYTES.div_ceil(3) * 4 {
            return Err(TrustError::TooLarge);
        }
        codec::decode_base64(&self.bundle).map_err(|e| TrustError::Json(e.to_string()))
    }
}

/// Parses a server id from untrusted text with the same rules as documents.
pub fn parse_server_id(s: &str) -> Option<Uuid> {
    parse_strict_uuid(s)
}

#[cfg(test)]
pub(crate) mod tests;
