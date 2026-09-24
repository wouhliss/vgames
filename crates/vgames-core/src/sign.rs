//! Domain-separated Ed25519 signatures (01-security §3.4), key ids and
//! fingerprints (§3.1), and the `vgames.sig/1` envelope.
//!
//! Every signature in vgames is pure Ed25519 over
//!
//! ```text
//! message = ASCII(context) || 0x00 || BLAKE3(payload_bytes)
//! ```
//!
//! where `payload_bytes` are the **exact** bytes of the signed document. The
//! context string binds a signature to one document type, so a trust-bundle
//! signature can never be replayed as a manifest signature. Verification is
//! always strict (`verify_strict`: canonical `S`, no small-order points).

use std::fmt;
use std::str::FromStr;

use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroizing;

use crate::codec::{self, CodecError, Digest};

pub const PUBLIC_KEY_LEN: usize = 32;
pub const SECRET_KEY_LEN: usize = 32;
pub const SIGNATURE_LEN: usize = 64;
pub const KEY_ID_LEN: usize = 16;
pub const FINGERPRINT_LEN: usize = 20;

/// `format` of a signature envelope.
pub const ENVELOPE_FORMAT: &str = "vgames.sig/1";
/// `alg` of a signature envelope.
pub const ENVELOPE_ALG: &str = "ed25519";
/// Envelopes are tiny (≈ 250 bytes); anything larger is refused unparsed.
pub const MAX_ENVELOPE_BYTES: usize = 4096;

/// The document type a signature is bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Context {
    /// `vgames.manifest/1` documents.
    #[serde(rename = "vgames/manifest/v1")]
    Manifest,
    /// `vgames.compat/1` profiles.
    #[serde(rename = "vgames/compat/v1")]
    Compat,
    /// `vgames.trust/1` bundles (root signatures).
    #[serde(rename = "vgames/trust/v1")]
    Trust,
}

impl Context {
    pub const ALL: [Context; 3] = [Context::Manifest, Context::Compat, Context::Trust];

    pub const fn as_str(self) -> &'static str {
        match self {
            Context::Manifest => "vgames/manifest/v1",
            Context::Compat => "vgames/compat/v1",
            Context::Trust => "vgames/trust/v1",
        }
    }
}

impl FromStr for Context {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        Context::ALL.into_iter().find(|c| c.as_str() == s).ok_or(())
    }
}

impl fmt::Display for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The bytes Ed25519 actually signs: `context || 0x00 || digest`.
pub fn signing_message(context: Context, payload_digest: &Digest) -> Vec<u8> {
    let ctx = context.as_str().as_bytes();
    let mut m = Vec::with_capacity(ctx.len() + 1 + codec::DIGEST_LEN);
    m.extend_from_slice(ctx);
    m.push(0);
    m.extend_from_slice(payload_digest.as_bytes());
    m
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("public key is not valid base64 of 32 bytes")]
    Encoding(#[from] CodecError),
    #[error("public key is not a valid Ed25519 point")]
    NotOnCurve,
    #[error("public key has small order")]
    WeakKey,
}

/// Why a signature did not verify. Callers map every variant to "refuse".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignatureError {
    #[error("signature context is {found}, expected {expected}")]
    WrongContext { expected: Context, found: Context },
    #[error("signature key id {found} does not match the verifying key {expected}")]
    KeyIdMismatch { expected: KeyId, found: KeyId },
    #[error("BLAKE3 of the payload does not match the envelope")]
    PayloadDigestMismatch,
    #[error("Ed25519 signature is invalid")]
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeError {
    #[error("signature envelope is larger than {MAX_ENVELOPE_BYTES} bytes")]
    TooLarge,
    #[error("signature envelope is not valid JSON: {0}")]
    Json(String),
    #[error("unknown signature envelope format {0:?}")]
    Format(String),
    #[error("unsupported signature algorithm {0:?}")]
    Alg(String),
    #[error("unknown signature context {0:?}")]
    Context(String),
    #[error("invalid {field}: {source}")]
    Field {
        field: &'static str,
        source: CodecError,
    },
}

/// The OS random source failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the operating system random number generator failed")]
pub struct RandomError;

/// Fills `buf` from the OS CSPRNG (`getrandom`), never a userspace PRNG.
pub fn fill_random(buf: &mut [u8]) -> Result<(), RandomError> {
    getrandom::fill(buf).map_err(|_| RandomError)
}

// ---------------------------------------------------------------- key ids

/// First 16 bytes of `BLAKE3(public_key)`, 32 lowercase hex characters on the wire.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyId([u8; KEY_ID_LEN]);

impl KeyId {
    pub const fn from_bytes(bytes: [u8; KEY_ID_LEN]) -> Self {
        Self(bytes)
    }
    pub const fn as_bytes(&self) -> &[u8; KEY_ID_LEN] {
        &self.0
    }
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl FromStr for KeyId {
    type Err = CodecError;
    fn from_str(s: &str) -> Result<Self, CodecError> {
        codec::decode_hex::<KEY_ID_LEN>(s).map(Self)
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyId({})", self.to_hex())
    }
}

impl Serialize for KeyId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for KeyId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------- fingerprints

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const FINGERPRINT_PREFIX: &str = "VG1";
/// Characters of base32 (160 bits / 5).
const FINGERPRINT_CHARS: usize = FINGERPRINT_LEN * 8 / 5;

/// The human-comparable form of a root key: `VG1-` + the first 20 bytes of
/// `BLAKE3(public_key)` in Crockford base32, grouped by 4
/// (`VG1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX`).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint([u8; FINGERPRINT_LEN]);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("expected a fingerprint like VG1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX")]
pub struct FingerprintError;

impl Fingerprint {
    pub const fn as_bytes(&self) -> &[u8; FINGERPRINT_LEN] {
        &self.0
    }

    fn base32(&self) -> String {
        let mut out = String::with_capacity(FINGERPRINT_CHARS);
        let mut acc: u32 = 0;
        let mut bits = 0u32;
        for &b in &self.0 {
            acc = (acc << 8) | u32::from(b);
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                let idx = ((acc >> bits) & 31) as usize;
                out.push(char::from(CROCKFORD.get(idx).copied().unwrap_or(b'0')));
            }
        }
        // 160 is a multiple of 5: no leftover bits.
        out
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(FINGERPRINT_PREFIX)?;
        let b32 = self.base32();
        for group in b32.as_bytes().chunks(4) {
            f.write_str("-")?;
            f.write_str(std::str::from_utf8(group).map_err(|_| fmt::Error)?)?;
        }
        Ok(())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({self})")
    }
}

/// Parses the canonical form only (uppercase, dashes every 4 characters), as
/// published in `/.well-known/vgames.json` and `vgames://server/add?fp=`.
impl FromStr for Fingerprint {
    type Err = FingerprintError;
    fn from_str(s: &str) -> Result<Self, FingerprintError> {
        let rest = s.strip_prefix(FINGERPRINT_PREFIX).ok_or(FingerprintError)?;
        if rest.len() != FINGERPRINT_CHARS / 4 * 5 {
            return Err(FingerprintError);
        }
        let mut acc: u32 = 0;
        let mut bits = 0u32;
        let mut out = [0u8; FINGERPRINT_LEN];
        let mut n = 0usize;
        for (i, c) in rest.bytes().enumerate() {
            if i % 5 == 0 {
                if c != b'-' {
                    return Err(FingerprintError);
                }
                continue;
            }
            let v = CROCKFORD
                .iter()
                .position(|&a| a == c)
                .ok_or(FingerprintError)?;
            acc = (acc << 5) | v as u32;
            bits += 5;
            if bits >= 8 {
                bits -= 8;
                let slot = out.get_mut(n).ok_or(FingerprintError)?;
                *slot = ((acc >> bits) & 0xff) as u8;
                n += 1;
            }
        }
        if n != FINGERPRINT_LEN {
            return Err(FingerprintError);
        }
        Ok(Self(out))
    }
}

impl Serialize for Fingerprint {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Fingerprint {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------- keys

/// An Ed25519 public key (root or publisher). Standard base64 on the wire.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PublicKey(VerifyingKey);

impl PublicKey {
    /// Rejects encodings that are not a curve point and small-order (weak) keys.
    pub fn from_bytes(bytes: &[u8; PUBLIC_KEY_LEN]) -> Result<Self, KeyError> {
        let key = VerifyingKey::from_bytes(bytes).map_err(|_| KeyError::NotOnCurve)?;
        if key.is_weak() {
            return Err(KeyError::WeakKey);
        }
        Ok(Self(key))
    }

    pub fn from_base64(s: &str) -> Result<Self, KeyError> {
        Self::from_bytes(&codec::decode_base64_exact::<PUBLIC_KEY_LEN>(s)?)
    }

    pub fn as_bytes(&self) -> &[u8; PUBLIC_KEY_LEN] {
        self.0.as_bytes()
    }

    pub fn to_base64(&self) -> String {
        codec::encode_base64(self.as_bytes())
    }

    /// First 16 bytes of `BLAKE3(public_key)`.
    pub fn key_id(&self) -> KeyId {
        let h = blake3::hash(self.as_bytes());
        let mut id = [0u8; KEY_ID_LEN];
        id.copy_from_slice(h.as_bytes().get(..KEY_ID_LEN).unwrap_or(&[0; KEY_ID_LEN]));
        KeyId(id)
    }

    /// `VG1-…`: first 20 bytes of `BLAKE3(public_key)` in grouped Crockford base32.
    pub fn fingerprint(&self) -> Fingerprint {
        let h = blake3::hash(self.as_bytes());
        let mut fp = [0u8; FINGERPRINT_LEN];
        fp.copy_from_slice(
            h.as_bytes()
                .get(..FINGERPRINT_LEN)
                .unwrap_or(&[0; FINGERPRINT_LEN]),
        );
        Fingerprint(fp)
    }

    /// Strict verification of `signature` over the domain-separated pre-hash.
    pub fn verify_digest(
        &self,
        context: Context,
        payload_digest: &Digest,
        signature: &Signature,
    ) -> Result<(), SignatureError> {
        verify_raw(self, &signing_message(context, payload_digest), signature)
    }

    /// Strict verification of `signature` over the exact `payload` bytes.
    pub fn verify(
        &self,
        context: Context,
        payload: &[u8],
        signature: &Signature,
    ) -> Result<(), SignatureError> {
        self.verify_digest(context, &Digest::of(payload), signature)
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.to_base64())
    }
}

impl Serialize for PublicKey {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_base64())
    }
}

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        Self::from_base64(&s).map_err(serde::de::Error::custom)
    }
}

/// An Ed25519 signature (64 bytes, standard base64 on the wire).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Signature([u8; SIGNATURE_LEN]);

impl Signature {
    pub const fn from_bytes(bytes: [u8; SIGNATURE_LEN]) -> Self {
        Self(bytes)
    }
    pub fn from_base64(s: &str) -> Result<Self, CodecError> {
        codec::decode_base64_exact::<SIGNATURE_LEN>(s).map(Self)
    }
    pub const fn as_bytes(&self) -> &[u8; SIGNATURE_LEN] {
        &self.0
    }
    pub fn to_base64(&self) -> String {
        codec::encode_base64(&self.0)
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signature({})", self.to_base64())
    }
}

impl Serialize for Signature {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_base64())
    }
}

impl<'de> Deserialize<'de> for Signature {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        Self::from_base64(&s).map_err(serde::de::Error::custom)
    }
}

/// An Ed25519 signing key. Zeroized on drop; never printed, cloned or serialized.
pub struct SecretKey(SigningKey);

impl SecretKey {
    /// A fresh key from the OS CSPRNG.
    pub fn generate() -> Result<Self, RandomError> {
        let mut seed = Zeroizing::new([0u8; SECRET_KEY_LEN]);
        fill_random(seed.as_mut())?;
        Ok(Self::from_seed(&seed))
    }

    /// The RFC 8032 32-byte private key ("seed").
    pub fn from_seed(seed: &[u8; SECRET_KEY_LEN]) -> Self {
        Self(SigningKey::from_bytes(seed))
    }

    /// The 32-byte seed, for encryption into a key file only.
    pub fn seed(&self) -> Zeroizing<[u8; SECRET_KEY_LEN]> {
        Zeroizing::new(self.0.to_bytes())
    }

    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.0.verifying_key())
    }

    /// Signs the domain-separated pre-hash of an already hashed payload (the
    /// browser worker receives only this digest).
    pub fn sign_digest(&self, context: Context, payload_digest: &Digest) -> Signature {
        sign_raw(self, &signing_message(context, payload_digest))
    }

    /// Signs the exact `payload` bytes under `context`.
    pub fn sign(&self, context: Context, payload: &[u8]) -> Signature {
        self.sign_digest(context, &Digest::of(payload))
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretKey")
            .field("key_id", &self.public_key().key_id())
            .finish_non_exhaustive()
    }
}

/// Plain Ed25519 (RFC 8032) over `message`. Only for the pre-hash construction
/// and the known-answer tests.
fn sign_raw(key: &SecretKey, message: &[u8]) -> Signature {
    Signature(key.0.sign(message).to_bytes())
}

fn verify_raw(key: &PublicKey, message: &[u8], sig: &Signature) -> Result<(), SignatureError> {
    let sig = ed25519_dalek::Signature::from_bytes(&sig.0);
    key.0
        .verify_strict(message, &sig)
        .map_err(|_| SignatureError::Invalid)
}

// ---------------------------------------------------------------- envelope

/// `vgames.sig/1`: a detached signature over a manifest or compat profile.
///
/// ```json
/// { "format": "vgames.sig/1", "alg": "ed25519", "context": "vgames/manifest/v1",
///   "key_id": "…", "payload_blake3": "…", "signature": "…" }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub context: Context,
    pub key_id: KeyId,
    pub payload_blake3: Digest,
    pub signature: Signature,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEnvelope<'a> {
    #[serde(borrow)]
    format: std::borrow::Cow<'a, str>,
    #[serde(borrow)]
    alg: std::borrow::Cow<'a, str>,
    #[serde(borrow)]
    context: std::borrow::Cow<'a, str>,
    #[serde(borrow)]
    key_id: std::borrow::Cow<'a, str>,
    #[serde(borrow)]
    payload_blake3: std::borrow::Cow<'a, str>,
    #[serde(borrow)]
    signature: std::borrow::Cow<'a, str>,
}

impl Envelope {
    /// Signs `payload` (exact bytes) and wraps the result.
    pub fn sign(key: &SecretKey, context: Context, payload: &[u8]) -> Self {
        Self::sign_digest(key, context, Digest::of(payload))
    }

    /// Signs an already computed payload digest and wraps the result.
    pub fn sign_digest(key: &SecretKey, context: Context, payload_blake3: Digest) -> Self {
        Self {
            context,
            key_id: key.public_key().key_id(),
            signature: key.sign_digest(context, &payload_blake3),
            payload_blake3,
        }
    }

    /// Parses envelope bytes strictly: known format/alg/context, no unknown
    /// fields, canonical hex and base64.
    pub fn parse(bytes: &[u8]) -> Result<Self, EnvelopeError> {
        if bytes.len() > MAX_ENVELOPE_BYTES {
            return Err(EnvelopeError::TooLarge);
        }
        let raw: RawEnvelope<'_> =
            serde_json::from_slice(bytes).map_err(|e| EnvelopeError::Json(e.to_string()))?;
        Self::from_raw(&raw)
    }

    fn from_raw(raw: &RawEnvelope<'_>) -> Result<Self, EnvelopeError> {
        if raw.format != ENVELOPE_FORMAT {
            return Err(EnvelopeError::Format(truncate(&raw.format)));
        }
        if raw.alg != ENVELOPE_ALG {
            return Err(EnvelopeError::Alg(truncate(&raw.alg)));
        }
        let context = raw
            .context
            .parse()
            .map_err(|()| EnvelopeError::Context(truncate(&raw.context)))?;
        let field = |field: &'static str| move |source| EnvelopeError::Field { field, source };
        Ok(Self {
            context,
            key_id: raw.key_id.parse().map_err(field("key_id"))?,
            payload_blake3: raw
                .payload_blake3
                .parse()
                .map_err(field("payload_blake3"))?,
            signature: Signature::from_base64(&raw.signature).map_err(field("signature"))?,
        })
    }

    /// Canonical JSON bytes (compact, fixed field order).
    pub fn to_bytes(&self) -> Vec<u8> {
        let raw = RawEnvelope {
            format: ENVELOPE_FORMAT.into(),
            alg: ENVELOPE_ALG.into(),
            context: self.context.as_str().into(),
            key_id: self.key_id.to_hex().into(),
            payload_blake3: self.payload_blake3.to_hex().into(),
            signature: self.signature.to_base64().into(),
        };
        // Serializing a struct of strings cannot fail.
        serde_json::to_vec(&raw).unwrap_or_default()
    }

    /// Verifies this envelope over the exact `payload` bytes with `key`:
    /// context, key id, payload digest, then strict Ed25519.
    pub fn verify(
        &self,
        key: &PublicKey,
        expected_context: Context,
        payload: &[u8],
    ) -> Result<(), SignatureError> {
        self.verify_digest(key, expected_context, &Digest::of(payload))
    }

    /// As [`Envelope::verify`], with the payload digest computed by the caller
    /// (for example while streaming a large manifest).
    pub fn verify_digest(
        &self,
        key: &PublicKey,
        expected_context: Context,
        payload_digest: &Digest,
    ) -> Result<(), SignatureError> {
        if self.context != expected_context {
            return Err(SignatureError::WrongContext {
                expected: expected_context,
                found: self.context,
            });
        }
        let key_id = key.key_id();
        if self.key_id != key_id {
            return Err(SignatureError::KeyIdMismatch {
                expected: key_id,
                found: self.key_id,
            });
        }
        if *payload_digest != self.payload_blake3 {
            return Err(SignatureError::PayloadDigestMismatch);
        }
        key.verify_digest(self.context, payload_digest, &self.signature)
    }
}

impl Serialize for Envelope {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        RawEnvelope {
            format: ENVELOPE_FORMAT.into(),
            alg: ENVELOPE_ALG.into(),
            context: self.context.as_str().into(),
            key_id: self.key_id.to_hex().into(),
            payload_blake3: self.payload_blake3.to_hex().into(),
            signature: self.signature.to_base64().into(),
        }
        .serialize(s)
    }
}

/// Envelopes embedded in API responses (`ReleaseDescriptor.signature`) use the
/// same strict rules as [`Envelope::parse`].
impl<'de> Deserialize<'de> for Envelope {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = RawEnvelope::deserialize(d)?;
        Self::from_raw(&raw).map_err(serde::de::Error::custom)
    }
}

/// Error messages echo at most a short prefix of attacker-controlled strings.
fn truncate(s: &str) -> String {
    s.chars().take(64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> SecretKey {
        SecretKey::from_seed(&[n; 32])
    }

    #[test]
    fn signing_message_layout() {
        let d = Digest::of(b"payload");
        let m = signing_message(Context::Manifest, &d);
        assert_eq!(&m[..18], b"vgames/manifest/v1");
        assert_eq!(m[18], 0);
        assert_eq!(&m[19..], d.as_bytes());
    }

    #[test]
    fn sign_verify_roundtrip() {
        let k = key(1);
        let sig = k.sign(Context::Manifest, b"hello");
        k.public_key()
            .verify(Context::Manifest, b"hello", &sig)
            .unwrap();
        assert_eq!(
            k.public_key().verify(Context::Manifest, b"hellp", &sig),
            Err(SignatureError::Invalid)
        );
        assert_eq!(
            key(2)
                .public_key()
                .verify(Context::Manifest, b"hello", &sig),
            Err(SignatureError::Invalid)
        );
    }

    #[test]
    fn contexts_never_cross_verify() {
        let k = key(3);
        for signed in Context::ALL {
            let sig = k.sign(signed, b"doc");
            for checked in Context::ALL {
                let r = k.public_key().verify(checked, b"doc", &sig);
                assert_eq!(r.is_ok(), signed == checked, "{signed} vs {checked}");
            }
        }
    }

    #[test]
    fn key_id_and_fingerprint_derivation() {
        let pk = key(4).public_key();
        let h = blake3::hash(pk.as_bytes());
        assert_eq!(pk.key_id().to_hex(), hex::encode(&h.as_bytes()[..16]));
        assert_eq!(pk.fingerprint().as_bytes(), &h.as_bytes()[..20]);
        let fp = pk.fingerprint().to_string();
        assert_eq!(fp.len(), 3 + 8 * 5);
        assert!(fp.starts_with("VG1-"));
        assert_eq!(fp.parse::<Fingerprint>().unwrap(), pk.fingerprint());
    }

    #[test]
    fn fingerprint_known_answer() {
        // All-zero input bits map to '0', all-one bits to 'Z'.
        assert_eq!(
            Fingerprint([0; 20]).to_string(),
            "VG1-0000-0000-0000-0000-0000-0000-0000-0000"
        );
        assert_eq!(
            Fingerprint([0xff; 20]).to_string(),
            "VG1-ZZZZ-ZZZZ-ZZZZ-ZZZZ-ZZZZ-ZZZZ-ZZZZ-ZZZZ"
        );
        // 0x00 0x44 0x32 0x14 0xc7 = 00000 00001 00010 00011 00100 00101 00110 00111
        let mut b = [0u8; 20];
        b[..5].copy_from_slice(&[0x00, 0x44, 0x32, 0x14, 0xc7]);
        assert!(Fingerprint(b).to_string().starts_with("VG1-0123-4567-"));
    }

    #[test]
    fn fingerprint_parse_is_canonical_only() {
        let fp = key(5).public_key().fingerprint().to_string();
        for bad in [
            fp.to_lowercase(),
            fp.replace('-', ""),
            fp.replacen("VG1", "VG2", 1),
            format!("{fp}-"),
            fp[..fp.len() - 1].to_string(),
            "VG1-IIII-0000-0000-0000-0000-0000-0000-0000".to_string(),
            "VG1-UUUU-0000-0000-0000-0000-0000-0000-0000".to_string(),
            String::new(),
        ] {
            assert!(bad.parse::<Fingerprint>().is_err(), "{bad}");
        }
    }

    #[test]
    fn weak_and_invalid_public_keys_are_rejected() {
        // The identity point (small order).
        let mut identity = [0u8; 32];
        identity[0] = 1;
        assert_eq!(PublicKey::from_bytes(&identity), Err(KeyError::WeakKey));
        // y = 2 is not on the curve.
        let mut bad = [0u8; 32];
        bad[0] = 2;
        assert_eq!(PublicKey::from_bytes(&bad), Err(KeyError::NotOnCurve));
        assert!(matches!(
            PublicKey::from_base64("AAAA"),
            Err(KeyError::Encoding(_))
        ));
    }

    #[test]
    fn envelope_roundtrip_and_verify() {
        let k = key(6);
        let env = Envelope::sign(&k, Context::Manifest, b"manifest bytes");
        let bytes = env.to_bytes();
        let parsed = Envelope::parse(&bytes).unwrap();
        assert_eq!(parsed, env);
        parsed
            .verify(&k.public_key(), Context::Manifest, b"manifest bytes")
            .unwrap();
        assert_eq!(
            parsed.verify(&k.public_key(), Context::Compat, b"manifest bytes"),
            Err(SignatureError::WrongContext {
                expected: Context::Compat,
                found: Context::Manifest
            })
        );
        assert_eq!(
            parsed.verify(&k.public_key(), Context::Manifest, b"other bytes"),
            Err(SignatureError::PayloadDigestMismatch)
        );
        assert!(matches!(
            parsed.verify(&key(7).public_key(), Context::Manifest, b"manifest bytes"),
            Err(SignatureError::KeyIdMismatch { .. })
        ));
        // serde path agrees with parse().
        let via_serde: Envelope = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(via_serde, env);
        assert_eq!(serde_json::to_vec(&env).unwrap(), bytes);
    }

    #[test]
    fn envelope_wire_format_snapshot() {
        // Ed25519 is deterministic, so the whole envelope is stable.
        let env = Envelope::sign(&key(6), Context::Manifest, b"manifest bytes");
        insta::assert_snapshot!(String::from_utf8(env.to_bytes()).unwrap());
        insta::assert_snapshot!(
            "root_fingerprint",
            key(6).public_key().fingerprint().to_string()
        );
    }

    #[test]
    fn envelope_rejects_malformed_fields() {
        let good: serde_json::Value =
            serde_json::from_slice(&Envelope::sign(&key(8), Context::Manifest, b"x").to_bytes())
                .unwrap();
        let mutate = |field: &str, value: serde_json::Value| {
            let mut v = good.clone();
            v[field] = value;
            Envelope::parse(&serde_json::to_vec(&v).unwrap())
        };
        assert!(matches!(
            mutate("format", "vgames.sig/2".into()),
            Err(EnvelopeError::Format(_))
        ));
        assert!(matches!(
            mutate("alg", "ed448".into()),
            Err(EnvelopeError::Alg(_))
        ));
        assert!(matches!(
            mutate("context", "vgames/other/v1".into()),
            Err(EnvelopeError::Context(_))
        ));
        assert!(matches!(
            mutate("key_id", "ABCDEF".into()),
            Err(EnvelopeError::Field {
                field: "key_id",
                ..
            })
        ));
        let upper = good["payload_blake3"].as_str().unwrap().to_uppercase();
        assert!(matches!(
            mutate("payload_blake3", upper.into()),
            Err(EnvelopeError::Field {
                field: "payload_blake3",
                ..
            })
        ));
        assert!(matches!(
            mutate("signature", "AAAA".into()),
            Err(EnvelopeError::Field {
                field: "signature",
                ..
            })
        ));
        assert!(matches!(
            mutate("extra", 1.into()),
            Err(EnvelopeError::Json(_))
        ));
        assert!(matches!(
            mutate("key_id", 5.into()),
            Err(EnvelopeError::Json(_))
        ));
        assert_eq!(
            Envelope::parse(&vec![b' '; MAX_ENVELOPE_BYTES + 1]),
            Err(EnvelopeError::TooLarge)
        );
    }

    #[test]
    fn non_canonical_signatures_are_rejected() {
        let k = key(9);
        let sig = k.sign(Context::Manifest, b"m");
        let pk = k.public_key();
        pk.verify(Context::Manifest, b"m", &sig).unwrap();

        // S' = S + L encodes the same scalar mod L; strict verification refuses it.
        const L: [u8; 32] = [
            0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9,
            0xde, 0x14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10,
        ];
        let mut bytes = *sig.as_bytes();
        let mut carry = 0u16;
        for i in 0..32 {
            let v = u16::from(bytes[32 + i]) + u16::from(L[i]) + carry;
            bytes[32 + i] = (v & 0xff) as u8;
            carry = v >> 8;
        }
        assert_eq!(carry, 0, "S + L fits in 256 bits");
        assert_eq!(
            pk.verify(Context::Manifest, b"m", &Signature::from_bytes(bytes)),
            Err(SignatureError::Invalid)
        );

        // A small-order R (the identity) with S = 0 is refused.
        let mut forged = [0u8; 64];
        forged[0] = 1;
        assert_eq!(
            pk.verify(Context::Manifest, b"m", &Signature::from_bytes(forged)),
            Err(SignatureError::Invalid)
        );
    }

    #[test]
    fn debug_never_prints_secret_material() {
        let k = key(10);
        let dbg = format!("{k:?}");
        assert!(!dbg.contains(&hex::encode(k.seed().as_ref())));
        assert!(dbg.contains(&k.public_key().key_id().to_hex()));
    }

    #[test]
    fn rfc8032_known_answers_via_raw_layer() {
        #[derive(serde::Deserialize)]
        struct Case {
            name: String,
            secret_key: String,
            public_key: String,
            message: String,
            signature: String,
        }
        #[derive(serde::Deserialize)]
        struct File {
            cases: Vec<Case>,
        }
        let file: File =
            serde_json::from_str(include_str!("../tests/vectors/rfc8032_ed25519.json")).unwrap();
        assert_eq!(file.cases.len(), 5);
        for c in file.cases {
            let seed: [u8; 32] = hex::decode(&c.secret_key).unwrap().try_into().unwrap();
            let sk = SecretKey::from_seed(&seed);
            assert_eq!(
                hex::encode(sk.public_key().as_bytes()),
                c.public_key,
                "{}",
                c.name
            );
            let msg = hex::decode(&c.message).unwrap();
            let sig = sign_raw(&sk, &msg);
            assert_eq!(hex::encode(sig.as_bytes()), c.signature, "{}", c.name);
            verify_raw(&sk.public_key(), &msg, &sig).unwrap();
        }
    }
}
