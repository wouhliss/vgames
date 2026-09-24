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
//! - [`layout`] — the single chunk/pack layout function (02 §4).
//! - [`manifest`] — `vgames.manifest/1` types and `parse_and_validate` (02 §5).
//! - [`paths`] — package path safety rules (02 §3), exact Unicode simple case folding.
//! - [`sign`]  — domain-separated Ed25519 signatures, key ids, `VG1-…` fingerprints,
//!   `vgames.sig/1` envelopes.
//!
//! Rules: no filesystem or network I/O, no clock reads (callers pass `now`),
//! no `unsafe`, no panics on untrusted input, WASM-compatible.

#![forbid(unsafe_code)]

pub mod codec;
pub mod layout;
pub mod manifest;
pub mod paths;
pub mod sign;

pub use codec::{Digest, Timestamp};
pub use sign::{Context, Envelope, Fingerprint, KeyId, PublicKey, SecretKey, Signature};
