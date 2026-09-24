//! Browser exports (feature `wasm`) for the admin-web signing Web Worker
//! (01-security §3.3). The private key is decrypted into WASM memory and never
//! leaves it: JavaScript gets an opaque [`UnlockedKey`] handle whose only
//! operations sign a 32-byte digest and return a public signature envelope.
//! Call `free()` on the handle (or terminate the worker) when the upload ends;
//! the key is zeroized on drop.
//!
//! ```js
//! import init, { keyfileInfo, UnlockedKey, fingerprint } from "@vgames/pack-wasm";
//! const info = keyfileInfo(fileBytes);            // { kind, keyId, label, … } — no passphrase needed
//! const key = UnlockedKey.unlock(fileBytes, passphrase);  // throws "wrong passphrase", …
//! const envelopeJson = key.signManifestDigest(manifestBlake3Hex);
//! key.free();
//! ```

use wasm_bindgen::prelude::*;

use crate::codec::Digest;
use crate::keyfile::{KeyFile, KeyKind};
use crate::sign::{Context, Envelope, PublicKey, SecretKey};

fn js_err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

/// Public header of a key file, readable without the passphrase.
#[wasm_bindgen(getter_with_clone)]
pub struct KeyfileInfo {
    /// `"root"` or `"publisher"`.
    pub kind: String,
    #[wasm_bindgen(js_name = keyId)]
    pub key_id: String,
    #[wasm_bindgen(js_name = publicKey)]
    pub public_key: String,
    pub label: String,
    #[wasm_bindgen(js_name = createdAt)]
    pub created_at: String,
}

/// Reads the public header of a `vgames.key/1` file.
#[wasm_bindgen(js_name = keyfileInfo)]
pub fn keyfile_info(keyfile: &[u8]) -> Result<KeyfileInfo, JsError> {
    let f = KeyFile::parse(keyfile).map_err(js_err)?;
    Ok(KeyfileInfo {
        kind: f.kind.as_str().to_owned(),
        key_id: f.key_id.to_hex(),
        public_key: f.public_key.to_base64(),
        label: f.label.clone(),
        created_at: f.created_at.to_string(),
    })
}

/// `VG1-…` fingerprint of a base64 Ed25519 public key.
#[wasm_bindgen]
pub fn fingerprint(public_key_base64: &str) -> Result<String, JsError> {
    let pk = PublicKey::from_base64(public_key_base64).map_err(js_err)?;
    Ok(pk.fingerprint().to_string())
}

/// A decrypted publisher key living only inside WASM memory.
#[wasm_bindgen]
pub struct UnlockedKey {
    key: SecretKey,
}

#[wasm_bindgen]
impl UnlockedKey {
    /// Decrypts a **publisher** key file (root keys are for the offline CLI only).
    pub fn unlock(keyfile: &[u8], passphrase: &str) -> Result<UnlockedKey, JsError> {
        let f = KeyFile::parse(keyfile).map_err(js_err)?;
        if f.kind != KeyKind::Publisher {
            return Err(JsError::new(
                "this is a root key: sign trust bundles with the offline CLI, not in a browser",
            ));
        }
        let key = f.decrypt(passphrase.as_bytes()).map_err(js_err)?;
        Ok(UnlockedKey { key })
    }

    #[wasm_bindgen(getter, js_name = keyId)]
    pub fn key_id(&self) -> String {
        self.key.public_key().key_id().to_hex()
    }

    /// Signs a manifest's BLAKE3 (64 lowercase hex) and returns the
    /// `vgames.sig/1` envelope JSON (the `manifest.sig` bytes).
    #[wasm_bindgen(js_name = signManifestDigest)]
    pub fn sign_manifest_digest(&self, blake3_hex: &str) -> Result<String, JsError> {
        self.sign(Context::Manifest, blake3_hex)
    }

    /// Signs a compat profile's BLAKE3 and returns the envelope JSON.
    #[wasm_bindgen(js_name = signCompatDigest)]
    pub fn sign_compat_digest(&self, blake3_hex: &str) -> Result<String, JsError> {
        self.sign(Context::Compat, blake3_hex)
    }

    fn sign(&self, context: Context, blake3_hex: &str) -> Result<String, JsError> {
        let digest: Digest = blake3_hex.parse().map_err(js_err)?;
        let env = Envelope::sign_digest(&self.key, context, digest);
        String::from_utf8(env.to_bytes()).map_err(js_err)
    }
}
