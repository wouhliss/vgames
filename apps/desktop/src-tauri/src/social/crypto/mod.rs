//! End-to-end encryption primitives (05-social §4), wrapping **vodozemac 0.11** Olm.
//!
//! Everything here is pure: no I/O, no clock, no database. The store (`social::store`)
//! loads pickles, calls these functions and stores the results in one transaction.
//!
//! - [`OlmAccount`]: a device's identity (Curve25519) and signing (Ed25519) keys, its
//!   signed one-time keys and fallback key, and inbound session creation.
//! - [`OlmSession`]: one pairwise Double Ratchet with a peer device.
//! - Signatures over the canonical JSON in `vgames_proto::social::canonical`, checked with
//!   Ed25519 strict verification ([`verify_device_keys`], [`verify_one_time_key`]).
//! - [`safety_number`]: the 60-digit number contacts compare out of band (05-social-notes §2.2).
//! - [`BodyCipher`]: XChaCha20-Poly1305 for message bodies at rest (05-social §4.3).
//!
//! Pickles are encrypted with a 32-byte key from the OS keychain ([`SecretKey32`]).

use base64::Engine as _;
use chacha20poly1305::aead::AeadInOut;
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use uuid::Uuid;
use vgames_proto::social::{
    ClaimedKey, DeviceKeys, PUBLIC_KEY_B64_LEN, SIGNATURE_B64_LEN, SignedOneTimeKey, canonical,
    is_unpadded_b64,
};
use vodozemac::olm::{
    Account, AccountPickle, OlmMessage, PreKeyMessage, Session, SessionConfig, SessionPickle,
};
use vodozemac::{Curve25519PublicKey, Ed25519PublicKey, Ed25519Signature};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// One-time keys uploaded at registration (05-social §4.1).
pub const INITIAL_ONE_TIME_KEYS: usize = 50;
/// Top up when the server reports fewer than this many unclaimed keys.
pub const ONE_TIME_KEY_LOW_WATER: usize = 20;
/// vodozemac numbers fallback keys and one-time keys with separate counters, so their ids
/// collide; the server keys both by `(device, key_id)`. Fallback key ids get this prefix
/// (the id is only a label: sessions find the private key by its public key).
pub const FALLBACK_KEY_ID_PREFIX: &str = "F";

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("a public key is malformed")]
    BadKey,
    #[error("a signature is malformed or does not verify")]
    BadSignature,
    #[error("the device keys were signed for a different user or server")]
    WrongBinding,
    #[error("the Olm message is malformed")]
    BadMessage,
    #[error("the Olm message does not decrypt (tampered, replayed or for another session)")]
    Decrypt,
    #[error("no session can decrypt this message")]
    NoSession,
    #[error("the pre-key message uses an unknown or already used one-time key")]
    UnknownOneTimeKey,
    #[error("the pickle cannot be decrypted with this key")]
    Pickle,
    #[error("the encrypted body does not authenticate")]
    Body,
    #[error("the key exchange was insecure")]
    Insecure,
}

/// A 32-byte secret from the OS keychain (pickle key or chat-database key). Zeroized on drop.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SecretKey32([u8; 32]);

impl SecretKey32 {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// A fresh random key from the OS CSPRNG.
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut k = [0u8; 32];
        getrandom::fill(&mut k)?;
        Ok(Self(k))
    }

    pub fn expose(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for SecretKey32 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretKey32(…)")
    }
}

fn curve_key(s: &str) -> Result<Curve25519PublicKey, CryptoError> {
    if !is_unpadded_b64(s, PUBLIC_KEY_B64_LEN) {
        return Err(CryptoError::BadKey);
    }
    Curve25519PublicKey::from_base64(s).map_err(|_| CryptoError::BadKey)
}

fn ed_key(s: &str) -> Result<Ed25519PublicKey, CryptoError> {
    if !is_unpadded_b64(s, PUBLIC_KEY_B64_LEN) {
        return Err(CryptoError::BadKey);
    }
    Ed25519PublicKey::from_base64(s).map_err(|_| CryptoError::BadKey)
}

fn verify(signing_key: &str, message: &str, signature: &str) -> Result<(), CryptoError> {
    let key = ed_key(signing_key)?;
    if !is_unpadded_b64(signature, SIGNATURE_B64_LEN) {
        return Err(CryptoError::BadSignature);
    }
    let sig = Ed25519Signature::from_base64(signature).map_err(|_| CryptoError::BadSignature)?;
    key.verify(message.as_bytes(), &sig)
        .map_err(|_| CryptoError::BadSignature)
}

// ---------------------------------------------------------------------------------------
// Account
// ---------------------------------------------------------------------------------------

/// Public keys and self-signature for `POST /v1/devices`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedDeviceKeys {
    pub identity_key: String,
    pub signing_key: String,
    pub keys_signature: String,
}

/// A device's Olm account.
pub struct OlmAccount {
    inner: Account,
}

impl Default for OlmAccount {
    fn default() -> Self {
        Self::new()
    }
}

impl OlmAccount {
    /// New random identity and signing keys.
    pub fn new() -> Self {
        Self {
            inner: Account::new(),
        }
    }

    pub fn identity_key(&self) -> String {
        self.inner.curve25519_key().to_base64()
    }

    pub fn signing_key(&self) -> String {
        self.inner.ed25519_key().to_base64()
    }

    fn sign(&self, message: &str) -> String {
        self.inner.sign(message).to_base64()
    }

    /// Identity keys with the self-signature over the canonical JSON (05-social-notes §2.1).
    pub fn signed_device_keys(&self, user_id: Uuid, server_id: Uuid) -> SignedDeviceKeys {
        let identity_key = self.identity_key();
        let signing_key = self.signing_key();
        let message = canonical::device_keys(&identity_key, server_id, &signing_key, user_id);
        SignedDeviceKeys {
            keys_signature: self.sign(&message),
            identity_key,
            signing_key,
        }
    }

    /// Generates `count` new one-time keys. They stay "unpublished" until
    /// [`OlmAccount::mark_keys_as_published`] is called after a successful upload.
    pub fn generate_one_time_keys(&mut self, count: usize) {
        let _ = self.inner.generate_one_time_keys(count);
    }

    /// Rotates the fallback key (the previous one stays usable until the next rotation).
    pub fn generate_fallback_key(&mut self) {
        let _ = self.inner.generate_fallback_key();
    }

    /// Unpublished one-time keys and fallback key, signed, ready for upload.
    pub fn unpublished_keys(&self) -> (Vec<SignedOneTimeKey>, Option<SignedOneTimeKey>) {
        let mut otks: Vec<SignedOneTimeKey> = self
            .inner
            .one_time_keys()
            .into_iter()
            .map(|(id, key)| self.signed_key(false, &id.to_base64(), &key.to_base64()))
            .collect();
        otks.sort_by(|a, b| a.key_id.cmp(&b.key_id));
        let fallback = self
            .inner
            .fallback_key()
            .into_iter()
            .next()
            .map(|(id, key)| {
                self.signed_key(
                    true,
                    &format!("{FALLBACK_KEY_ID_PREFIX}{}", id.to_base64()),
                    &key.to_base64(),
                )
            });
        (otks, fallback)
    }

    fn signed_key(&self, fallback: bool, key_id: &str, public_key: &str) -> SignedOneTimeKey {
        SignedOneTimeKey {
            key_id: key_id.to_owned(),
            public_key: public_key.to_owned(),
            signature: self.sign(&canonical::one_time_key(fallback, public_key, key_id)),
        }
    }

    pub fn mark_keys_as_published(&mut self) {
        self.inner.mark_keys_as_published();
    }

    /// Private one-time keys still held (published or not).
    pub fn stored_one_time_keys(&self) -> usize {
        self.inner.stored_one_time_key_count()
    }

    /// Starts a session with `peer` using a key claimed from the server. The claimed key's
    /// signature is verified against the peer's **pinned** signing key first.
    pub fn create_outbound_session(
        &self,
        peer: &PinnedDevice,
        claimed: &ClaimedKey,
    ) -> Result<OlmSession, CryptoError> {
        verify_one_time_key(&peer.signing_key, claimed)?;
        let identity = curve_key(&peer.identity_key)?;
        let otk = curve_key(&claimed.public_key)?;
        let session = self
            .inner
            .create_outbound_session(SessionConfig::version_1(), identity, otk)
            .map_err(|_| CryptoError::Insecure)?;
        Ok(OlmSession { inner: session })
    }

    /// Creates the inbound session a pre-key message establishes and returns its plaintext.
    /// Consumes the one-time key it used (a fallback key stays until rotated).
    pub fn create_inbound_session(
        &mut self,
        sender_identity_key: &str,
        message: &OlmMessage,
    ) -> Result<(OlmSession, Vec<u8>), CryptoError> {
        let OlmMessage::PreKey(pre_key) = message else {
            return Err(CryptoError::NoSession);
        };
        let identity = curve_key(sender_identity_key)?;
        match self
            .inner
            .create_inbound_session(SessionConfig::version_1(), identity, pre_key)
        {
            Ok(r) => Ok((OlmSession { inner: r.session }, r.plaintext)),
            Err(vodozemac::olm::SessionCreationError::MissingOneTimeKey(_)) => {
                Err(CryptoError::UnknownOneTimeKey)
            }
            Err(vodozemac::olm::SessionCreationError::Decryption(_))
            | Err(vodozemac::olm::SessionCreationError::MismatchedIdentityKey(..)) => {
                Err(CryptoError::Decrypt)
            }
            Err(_) => Err(CryptoError::Insecure),
        }
    }

    /// Serializes and encrypts the account with the keychain pickle key.
    pub fn pickle(&self, key: &SecretKey32) -> String {
        self.inner.pickle().encrypt(key.expose())
    }

    pub fn unpickle(pickle: &str, key: &SecretKey32) -> Result<Self, CryptoError> {
        let p =
            AccountPickle::from_encrypted(pickle, key.expose()).map_err(|_| CryptoError::Pickle)?;
        Ok(Self {
            inner: Account::from_pickle(p),
        })
    }
}

// ---------------------------------------------------------------------------------------
// Sessions and messages
// ---------------------------------------------------------------------------------------

/// Olm message type on the wire (`olm_message_type`).
pub const MESSAGE_TYPE_PRE_KEY: u8 = 0;
pub const MESSAGE_TYPE_NORMAL: u8 = 1;

/// Parses wire parts into an Olm message.
pub fn parse_message(message_type: u8, ciphertext: &[u8]) -> Result<OlmMessage, CryptoError> {
    OlmMessage::from_parts(usize::from(message_type), ciphertext)
        .map_err(|_| CryptoError::BadMessage)
}

/// The session id a pre-key message belongs to (to reuse an existing inbound session).
pub fn pre_key_session_id(message: &OlmMessage) -> Option<String> {
    match message {
        OlmMessage::PreKey(p) => Some(PreKeyMessage::session_id(p)),
        OlmMessage::Normal(_) => None,
    }
}

/// One pairwise Olm session.
pub struct OlmSession {
    inner: Session,
}

impl OlmSession {
    /// Globally unique session id (base64).
    pub fn session_id(&self) -> String {
        self.inner.session_id()
    }

    /// Whether the peer has answered on this session (after that, messages are "normal").
    pub fn has_received_message(&self) -> bool {
        self.inner.has_received_message()
    }

    /// Encrypts `plaintext`; returns `(olm_message_type, message bytes)`.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<(u8, Vec<u8>), CryptoError> {
        let message = self
            .inner
            .encrypt(plaintext)
            .map_err(|_| CryptoError::Insecure)?;
        let (kind, bytes) = message.to_parts();
        let kind = u8::try_from(kind).map_err(|_| CryptoError::BadMessage)?;
        Ok((kind, bytes))
    }

    pub fn decrypt(&mut self, message: &OlmMessage) -> Result<Vec<u8>, CryptoError> {
        self.inner
            .decrypt(message)
            .map_err(|_| CryptoError::Decrypt)
    }

    pub fn pickle(&self, key: &SecretKey32) -> String {
        self.inner.pickle().encrypt(key.expose())
    }

    pub fn unpickle(pickle: &str, key: &SecretKey32) -> Result<Self, CryptoError> {
        let p =
            SessionPickle::from_encrypted(pickle, key.expose()).map_err(|_| CryptoError::Pickle)?;
        Ok(Self {
            inner: Session::from_pickle(p),
        })
    }
}

// ---------------------------------------------------------------------------------------
// Device key verification (TOFU pins)
// ---------------------------------------------------------------------------------------

/// A contact device whose self-signature verified. Pinned on first sight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedDevice {
    pub device_id: Uuid,
    pub user_id: Uuid,
    pub identity_key: String,
    pub signing_key: String,
}

/// Verifies a device's self-signature and its binding to `user_id` on `server_id`.
pub fn verify_device_keys(
    keys: &DeviceKeys,
    user_id: Uuid,
    server_id: Uuid,
) -> Result<PinnedDevice, CryptoError> {
    curve_key(&keys.identity_key)?;
    let message = canonical::device_keys(&keys.identity_key, server_id, &keys.signing_key, user_id);
    verify(&keys.signing_key, &message, &keys.keys_signature).map_err(|e| match e {
        // A valid signature over other bindings still fails to verify here; report it as such.
        CryptoError::BadSignature => CryptoError::WrongBinding,
        other => other,
    })?;
    Ok(PinnedDevice {
        device_id: keys.device_id,
        user_id,
        identity_key: keys.identity_key.clone(),
        signing_key: keys.signing_key.clone(),
    })
}

/// Verifies a claimed one-time or fallback key against the device's signing key.
pub fn verify_one_time_key(signing_key: &str, claimed: &ClaimedKey) -> Result<(), CryptoError> {
    curve_key(&claimed.public_key)?;
    let message =
        canonical::one_time_key(claimed.is_fallback, &claimed.public_key, &claimed.key_id);
    verify(signing_key, &message, &claimed.signature)
}

// ---------------------------------------------------------------------------------------
// Safety numbers (05-social-notes §2.2)
// ---------------------------------------------------------------------------------------

const SAFETY_USER_CONTEXT: &str = "vgames 2026-09 safety number user v1";
const SAFETY_CONTEXT: &str = "vgames 2026-09 safety number v1";

/// 60 digits in 12 groups of 5.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafetyNumber {
    pub groups: Vec<String>,
}

impl SafetyNumber {
    pub fn digits(&self) -> String {
        self.groups.concat()
    }
}

/// Decoded `(identity_key, signing_key)` pairs of one user's devices.
fn user_fingerprint(user_id: Uuid, devices: &[(String, String)]) -> Result<[u8; 32], CryptoError> {
    let mut keys: Vec<[u8; 64]> = Vec::with_capacity(devices.len());
    for (identity, signing) in devices {
        let mut k = [0u8; 64];
        k[..32].copy_from_slice(curve_key(identity)?.as_bytes());
        k[32..].copy_from_slice(ed_key(signing)?.as_bytes());
        keys.push(k);
    }
    keys.sort_unstable();
    keys.dedup();
    let mut input = Vec::with_capacity(16 + 4 + keys.len() * 64);
    input.extend_from_slice(user_id.as_bytes());
    input.extend_from_slice(&u32::try_from(keys.len()).unwrap_or(u32::MAX).to_be_bytes());
    for k in &keys {
        input.extend_from_slice(k);
    }
    Ok(blake3::derive_key(SAFETY_USER_CONTEXT, &input))
}

/// The safety number for two users, each with their known non-revoked devices'
/// `(identity_key, signing_key)`. Symmetric: both sides compute the same digits.
pub fn safety_number(
    a: (Uuid, &[(String, String)]),
    b: (Uuid, &[(String, String)]),
) -> Result<SafetyNumber, CryptoError> {
    let (low, high) = if a.0.as_bytes() <= b.0.as_bytes() {
        (a, b)
    } else {
        (b, a)
    };
    let mut hasher = blake3::Hasher::new_derive_key(SAFETY_CONTEXT);
    hasher.update(&user_fingerprint(low.0, low.1)?);
    hasher.update(&user_fingerprint(high.0, high.1)?);
    let mut out = [0u8; 60];
    hasher.finalize_xof().fill(&mut out);
    let (chunks, _) = out.as_chunks::<5>();
    let groups = chunks
        .iter()
        .map(|c| {
            let mut v = 0u64;
            for byte in c {
                v = (v << 8) | u64::from(*byte);
            }
            format!("{:05}", v % 100_000)
        })
        .collect();
    Ok(SafetyNumber { groups })
}

/// A short, grouped rendering of an identity key for device lists ("AB12 CD34 …").
pub fn key_fingerprint(identity_key: &str) -> String {
    let digest = blake3::hash(identity_key.as_bytes());
    let hex = digest.to_hex();
    hex.as_str()
        .as_bytes()
        .chunks(4)
        .take(6)
        .map(|c| String::from_utf8_lossy(c).to_uppercase())
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------------------
// Bodies at rest
// ---------------------------------------------------------------------------------------

pub const BODY_NONCE_LEN: usize = 24;

/// XChaCha20-Poly1305 under the per-install chat key, with the row identity as associated
/// data (a body copied to another row does not decrypt).
pub struct BodyCipher {
    cipher: XChaCha20Poly1305,
}

impl BodyCipher {
    pub fn new(key: &SecretKey32) -> Result<Self, CryptoError> {
        let cipher =
            XChaCha20Poly1305::new_from_slice(key.expose()).map_err(|_| CryptoError::Body)?;
        Ok(Self { cipher })
    }

    /// Returns `(nonce, ciphertext)`.
    pub fn seal(
        &self,
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<([u8; BODY_NONCE_LEN], Vec<u8>), CryptoError> {
        let mut nonce = [0u8; BODY_NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_| CryptoError::Body)?;
        let mut buf = plaintext.to_vec();
        self.cipher
            .encrypt_in_place(&XNonce::from(nonce), aad, &mut buf)
            .map_err(|_| CryptoError::Body)?;
        Ok((nonce, buf))
    }

    pub fn open(
        &self,
        aad: &[u8],
        nonce: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        let nonce: [u8; BODY_NONCE_LEN] = nonce.try_into().map_err(|_| CryptoError::Body)?;
        let mut buf = ciphertext.to_vec();
        self.cipher
            .decrypt_in_place(&XNonce::from(nonce), aad, &mut buf)
            .map_err(|_| CryptoError::Body)?;
        Ok(buf)
    }
}

/// Standard base64 with padding (the API's binary encoding, 03-api §1).
pub fn wire_b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn wire_b64_decode(s: &str) -> Result<Vec<u8>, CryptoError> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|_| CryptoError::BadMessage)
}

#[cfg(test)]
mod tests;
