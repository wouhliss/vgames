//! # vgames-core
//!
//! The single implementation of every security-critical format in vgames. The
//! API server, the desktop launcher, the `vgames` CLI and the admin-web packer
//! (compiled to WASM) all link this crate, so a format can never be parsed two
//! different ways.
//!
//! Owner: Agent 5 (DevOps & Security). Spec: `docs/architecture/01-security.md`
//! and `docs/architecture/02-package-format.md`.
//!
//! Modules:
//! - [`codec`] — strict wire encodings: BLAKE3 digests (lowercase hex), base64, RFC 3339 UTC.
//! - [`compat`] — `vgames.compat/1` profiles (09-compatibility §4): env/DLL rules, winetricks allowlist.
//! - [`keyfile`] — `vgames.key/1` encrypted key files (Argon2id + XChaCha20-Poly1305).
//! - [`layout`] — the single chunk/pack layout function (02 §4).
//! - [`manifest`] — `vgames.manifest/1` types and `parse_and_validate` (02 §5).
//! - [`paths`] — package path safety rules (02 §3), exact Unicode simple case folding.
//! - [`runtimes`] — the signed `vgames.runtimes/1` runtime catalog (09-compatibility §5), `verify_catalog`.
//! - [`sign`]  — domain-separated Ed25519 signatures, key ids, `VG1-…` fingerprints,
//!   `vgames.sig/1` envelopes.
//! - [`trust`] — `vgames.trust/1` bundles, `verify_bundle`, root rotation, `TrustState`.
//! - `wasm` (feature `wasm`) — browser worker exports: unlock a key file, sign a digest, fingerprint.
//! - [`verify`] — `verify_manifest` (01-security §3.4 steps 1–6) and `verify_compat_profile`.
//!
//! Rules: no filesystem or network I/O, no clock reads (callers pass `now`),
//! no `unsafe`, no panics on untrusted input, WASM-compatible.

#![forbid(unsafe_code)]

pub mod codec;
pub mod compat;
pub mod keyfile;
pub mod layout;
pub mod manifest;
pub mod paths;
pub mod runtimes;
pub mod sign;
pub mod trust;
pub mod verify;
#[cfg(feature = "wasm")]
pub mod wasm;

pub use codec::{Digest, Timestamp};
pub use sign::{Context, Envelope, Fingerprint, KeyId, PublicKey, SecretKey, Signature};
