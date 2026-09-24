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
//! Planned modules:
//! - `manifest`   — `vgames.manifest/1` types, parse + structural validation.
//! - `paths`      — install-path safety rules (traversal, reserved names, case collisions).
//! - `sign`       — domain-separated Ed25519 signing/verification over BLAKE3 pre-hashes.
//! - `trust`      — root-signed trust bundles, publisher certificates, revocation.
//! - `keyfile`    — `vgames.key/1` encrypted key files (Argon2id + XChaCha20-Poly1305).
//! - `fingerprint`— human-readable key fingerprints shown in the UI.
//!
//! Rules: no filesystem or network I/O, no `unsafe`, no panics on untrusted input.
