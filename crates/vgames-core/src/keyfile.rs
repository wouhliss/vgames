//! `vgames.key/1`: an Ed25519 private key encrypted at rest (01-security §2, §3.1, §3.3).
//!
//! ```json
//! {
//!   "format": "vgames.key/1",
//!   "kind": "publisher",
//!   "key_id": "…32 hex…",
//!   "public_key": "…base64…",
//!   "label": "alice@workstation",
//!   "created_at": "2026-09-24T10:00:00Z",
//!   "kdf": { "alg": "argon2id", "m_kib": 65536, "t": 3, "p": 1, "salt": "…16 bytes…" },
//!   "cipher": { "alg": "xchacha20poly1305", "nonce": "…24 bytes…" },
//!   "check": "…16 bytes…",
//!   "ciphertext": "…32-byte seed + 16-byte tag…"
//! }
//! ```
//!
//! - Argon2id(passphrase, salt), m = 64 MiB, t = 3, p = 1, 64 output bytes:
//!   the first 32 are the XChaCha20-Poly1305 key, the last 32 derive `check`.
//!   Parameters are fixed by the format: a file that declares others is refused
//!   (so a hostile file cannot make the reader allocate gigabytes).
//! - `check` = first 16 bytes of `BLAKE3-keyed(check_key, "vgames.key/1 passphrase check")`.
//!   It tells a wrong passphrase apart from a damaged file. It does not help an
//!   attacker: testing a guess costs one Argon2id run either way.
//! - The AEAD's associated data is every header field (length-prefixed), so
//!   changing `kind`, `key_id`, `public_key`, `label`, `created_at` or the nonce
//!   makes decryption fail with [`KeyFileError::AuthenticationFailed`]. Changing
//!   the salt changes the derived key and reads as a wrong passphrase.
//! - Decryption returns a [`SecretKey`] (zeroized on drop) and checks that it
//!   matches the public key in the header.

use chacha20poly1305::aead::AeadInOut;
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::codec::{self, CodecError, Timestamp};
use crate::sign::{self, KeyId, PublicKey, SECRET_KEY_LEN, SecretKey};

pub const FORMAT: &str = "vgames.key/1";
pub const KDF_ALG: &str = "argon2id";
pub const KDF_M_KIB: u32 = 64 * 1024;
pub const KDF_T: u32 = 3;
pub const KDF_P: u32 = 1;
pub const CIPHER_ALG: &str = "xchacha20poly1305";
pub const SALT_LEN: usize = 16;
pub const NONCE_LEN: usize = 24;
pub const CHECK_LEN: usize = 16;
const TAG_LEN: usize = 16;
/// Key files are ~600 bytes; anything much larger is not one.
pub const MAX_KEYFILE_BYTES: usize = 16 * 1024;
pub const MAX_LABEL_CHARS: usize = 128;
const CHECK_CONTEXT: &[u8] = b"vgames.key/1 passphrase check";

/// What the key is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyKind {
    /// A server's root key: signs trust bundles. Kept offline.
    Root,
    /// A publisher key: signs manifests and compat profiles.
    Publisher,
}

impl KeyKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            KeyKind::Root => "root",
            KeyKind::Publisher => "publisher",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyFileError {
    #[error("the file is larger than {MAX_KEYFILE_BYTES} bytes and is not a vgames key file")]
    TooLarge,
    #[error("not a valid vgames.key/1 file: {0}")]
    Json(String),
    #[error("unknown key file format {0:?}")]
    Format(String),
    #[error("unsupported key derivation settings (only argon2id m=64 MiB t=3 p=1)")]
    UnsupportedKdf,
    #[error("unsupported cipher {0:?}")]
    UnsupportedCipher(String),
    #[error("invalid {field}: {source}")]
    Field {
        field: &'static str,
        source: CodecError,
    },
    #[error("key_id does not match the public key")]
    KeyIdMismatch,
    #[error("label must be 1 to {MAX_LABEL_CHARS} characters without control characters")]
    Label,
    #[error("wrong passphrase")]
    WrongPassphrase,
    #[error("the key file was modified or is damaged (authentication failed)")]
    AuthenticationFailed,
    #[error("the decrypted key does not match the public key in the file")]
    KeyMismatch,
    #[error("the operating system random number generator failed")]
    Random,
    #[error("key derivation failed")]
    Kdf,
}

impl From<sign::RandomError> for KeyFileError {
    fn from(_: sign::RandomError) -> Self {
        KeyFileError::Random
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKdf {
    alg: String,
    m_kib: u32,
    t: u32,
    p: u32,
    salt: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCipher {
    alg: String,
    nonce: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKeyFile {
    format: String,
    kind: KeyKind,
    key_id: String,
    public_key: String,
    label: String,
    created_at: Timestamp,
    kdf: RawKdf,
    cipher: RawCipher,
    check: String,
    ciphertext: String,
}

/// A parsed key file. Holds only public data and ciphertext until [`KeyFile::decrypt`].
#[derive(Clone, PartialEq, Eq)]
pub struct KeyFile {
    pub kind: KeyKind,
    pub key_id: KeyId,
    pub public_key: PublicKey,
    pub label: String,
    pub created_at: Timestamp,
    salt: [u8; SALT_LEN],
    nonce: [u8; NONCE_LEN],
    check: [u8; CHECK_LEN],
    ciphertext: [u8; SECRET_KEY_LEN + TAG_LEN],
}

impl std::fmt::Debug for KeyFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyFile")
            .field("kind", &self.kind)
            .field("key_id", &self.key_id)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

fn valid_label(s: &str) -> bool {
    let n = s.chars().count();
    (1..=MAX_LABEL_CHARS).contains(&n) && !s.chars().any(char::is_control)
}

/// 32-byte AEAD key + 32-byte check key from the passphrase.
fn derive(passphrase: &[u8], salt: &[u8; SALT_LEN]) -> Result<Zeroizing<[u8; 64]>, KeyFileError> {
    let params =
        argon2::Params::new(KDF_M_KIB, KDF_T, KDF_P, Some(64)).map_err(|_| KeyFileError::Kdf)?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; 64]);
    argon
        .hash_password_into(passphrase, salt, out.as_mut())
        .map_err(|_| KeyFileError::Kdf)?;
    Ok(out)
}

fn split(okm: &[u8; 64]) -> (Zeroizing<[u8; 32]>, Zeroizing<[u8; 32]>) {
    let mut enc = Zeroizing::new([0u8; 32]);
    let mut chk = Zeroizing::new([0u8; 32]);
    let (a, b) = okm.split_at(32);
    enc.copy_from_slice(a);
    chk.copy_from_slice(b);
    (enc, chk)
}

fn check_value(check_key: &[u8; 32]) -> [u8; CHECK_LEN] {
    let h = blake3::keyed_hash(check_key, CHECK_CONTEXT);
    let mut out = [0u8; CHECK_LEN];
    out.copy_from_slice(h.as_bytes().get(..CHECK_LEN).unwrap_or(&[0; CHECK_LEN]));
    out
}

fn ct_eq_16(a: &[u8; 16], b: &[u8; 16]) -> bool {
    let mut x = [0u8; 32];
    let mut y = [0u8; 32];
    x[..16].copy_from_slice(a);
    y[..16].copy_from_slice(b);
    codec::ct_eq_32(&x, &y)
}

impl KeyFile {
    /// Encrypts `key` under `passphrase` with a fresh random salt and nonce.
    pub fn encrypt(
        key: &SecretKey,
        kind: KeyKind,
        label: &str,
        created_at: Timestamp,
        passphrase: &[u8],
    ) -> Result<Self, KeyFileError> {
        let mut salt = [0u8; SALT_LEN];
        let mut nonce = [0u8; NONCE_LEN];
        sign::fill_random(&mut salt)?;
        sign::fill_random(&mut nonce)?;
        Self::encrypt_with(key, kind, label, created_at, passphrase, salt, nonce)
    }

    /// [`KeyFile::encrypt`] with caller-provided salt and nonce (tests and
    /// known-answer vectors only; never reuse a salt or nonce).
    #[doc(hidden)]
    pub fn encrypt_with(
        key: &SecretKey,
        kind: KeyKind,
        label: &str,
        created_at: Timestamp,
        passphrase: &[u8],
        salt: [u8; SALT_LEN],
        nonce: [u8; NONCE_LEN],
    ) -> Result<Self, KeyFileError> {
        if !valid_label(label) {
            return Err(KeyFileError::Label);
        }
        let public_key = key.public_key();
        let mut file = KeyFile {
            kind,
            key_id: public_key.key_id(),
            public_key,
            label: label.to_owned(),
            created_at,
            salt,
            nonce,
            check: [0; CHECK_LEN],
            ciphertext: [0; SECRET_KEY_LEN + TAG_LEN],
        };
        let okm = derive(passphrase, &salt)?;
        let (enc_key, check_key) = split(&okm);
        file.check = check_value(&check_key);
        let cipher =
            XChaCha20Poly1305::new_from_slice(enc_key.as_ref()).map_err(|_| KeyFileError::Kdf)?;
        let mut buf: Zeroizing<Vec<u8>> =
            Zeroizing::new(Vec::with_capacity(SECRET_KEY_LEN + TAG_LEN));
        buf.extend_from_slice(key.seed().as_ref());
        cipher
            .encrypt_in_place(&XNonce::from(nonce), &file.aad(), &mut *buf)
            .map_err(|_| KeyFileError::AuthenticationFailed)?;
        file.ciphertext = buf
            .as_slice()
            .try_into()
            .map_err(|_| KeyFileError::AuthenticationFailed)?;
        Ok(file)
    }

    /// Associated data: every header field, each as a big-endian u32 length + bytes.
    fn aad(&self) -> Vec<u8> {
        let fields: [&[u8]; 12] = [
            FORMAT.as_bytes(),
            self.kind.as_str().as_bytes(),
            self.key_id.as_bytes(),
            self.public_key.as_bytes(),
            self.label.as_bytes(),
            &self.created_at.unix().to_be_bytes(),
            KDF_ALG.as_bytes(),
            &KDF_M_KIB.to_be_bytes(),
            &KDF_T.to_be_bytes(),
            &KDF_P.to_be_bytes(),
            CIPHER_ALG.as_bytes(),
            &self.nonce,
        ];
        let mut aad = Vec::with_capacity(256);
        for f in fields {
            aad.extend_from_slice(&(f.len() as u32).to_be_bytes());
            aad.extend_from_slice(f);
        }
        aad
    }

    /// Decrypts the private key. Distinguishes a wrong passphrase from a
    /// modified file.
    pub fn decrypt(&self, passphrase: &[u8]) -> Result<SecretKey, KeyFileError> {
        let okm = derive(passphrase, &self.salt)?;
        let (enc_key, check_key) = split(&okm);
        if !ct_eq_16(&check_value(&check_key), &self.check) {
            return Err(KeyFileError::WrongPassphrase);
        }
        let cipher =
            XChaCha20Poly1305::new_from_slice(enc_key.as_ref()).map_err(|_| KeyFileError::Kdf)?;
        let mut buf: Zeroizing<Vec<u8>> = Zeroizing::new(self.ciphertext.to_vec());
        cipher
            .decrypt_in_place(&XNonce::from(self.nonce), &self.aad(), &mut *buf)
            .map_err(|_| KeyFileError::AuthenticationFailed)?;
        let mut seed = Zeroizing::new([0u8; SECRET_KEY_LEN]);
        if buf.len() != SECRET_KEY_LEN {
            return Err(KeyFileError::AuthenticationFailed);
        }
        seed.copy_from_slice(&buf);
        let key = SecretKey::from_seed(&seed);
        if key.public_key() != self.public_key {
            return Err(KeyFileError::KeyMismatch);
        }
        Ok(key)
    }

    /// Parses key file bytes (no passphrase needed; nothing secret is exposed).
    pub fn parse(bytes: &[u8]) -> Result<Self, KeyFileError> {
        if bytes.len() > MAX_KEYFILE_BYTES {
            return Err(KeyFileError::TooLarge);
        }
        let raw: RawKeyFile =
            serde_json::from_slice(bytes).map_err(|e| KeyFileError::Json(e.to_string()))?;
        if raw.format != FORMAT {
            return Err(KeyFileError::Format(raw.format.chars().take(64).collect()));
        }
        if raw.kdf.alg != KDF_ALG
            || raw.kdf.m_kib != KDF_M_KIB
            || raw.kdf.t != KDF_T
            || raw.kdf.p != KDF_P
        {
            return Err(KeyFileError::UnsupportedKdf);
        }
        if raw.cipher.alg != CIPHER_ALG {
            return Err(KeyFileError::UnsupportedCipher(
                raw.cipher.alg.chars().take(64).collect(),
            ));
        }
        let field = |field: &'static str| move |source| KeyFileError::Field { field, source };
        let key_id: KeyId = raw.key_id.parse().map_err(field("key_id"))?;
        let public_key = PublicKey::from_base64(&raw.public_key).map_err(|e| match e {
            sign::KeyError::Encoding(source) => KeyFileError::Field {
                field: "public_key",
                source,
            },
            _ => KeyFileError::KeyMismatch,
        })?;
        if public_key.key_id() != key_id {
            return Err(KeyFileError::KeyIdMismatch);
        }
        if !valid_label(&raw.label) {
            return Err(KeyFileError::Label);
        }
        Ok(KeyFile {
            kind: raw.kind,
            key_id,
            public_key,
            label: raw.label,
            created_at: raw.created_at,
            salt: codec::decode_base64_exact(&raw.kdf.salt).map_err(field("kdf.salt"))?,
            nonce: codec::decode_base64_exact(&raw.cipher.nonce).map_err(field("cipher.nonce"))?,
            check: codec::decode_base64_exact(&raw.check).map_err(field("check"))?,
            ciphertext: codec::decode_base64_exact(&raw.ciphertext).map_err(field("ciphertext"))?,
        })
    }

    /// The file contents (pretty JSON). Write it with mode `0600`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let raw = RawKeyFile {
            format: FORMAT.into(),
            kind: self.kind,
            key_id: self.key_id.to_hex(),
            public_key: self.public_key.to_base64(),
            label: self.label.clone(),
            created_at: self.created_at,
            kdf: RawKdf {
                alg: KDF_ALG.into(),
                m_kib: KDF_M_KIB,
                t: KDF_T,
                p: KDF_P,
                salt: codec::encode_base64(&self.salt),
            },
            cipher: RawCipher {
                alg: CIPHER_ALG.into(),
                nonce: codec::encode_base64(&self.nonce),
            },
            check: codec::encode_base64(&self.check),
            ciphertext: codec::encode_base64(&self.ciphertext),
        };
        let mut out = serde_json::to_vec_pretty(&raw).unwrap_or_default();
        out.push(b'\n');
        out
    }
}

#[cfg(test)]
mod tests;
