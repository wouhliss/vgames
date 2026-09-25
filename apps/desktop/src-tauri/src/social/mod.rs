//! Social features in the launcher core (docs/architecture/05-social.md). Owner: Agent 4.
//!
//! - `crypto`: Olm (vodozemac) accounts, sessions, signatures, safety numbers, body cipher.
//! - `store`: the local E2EE state and message engine over SQLite (pins, sessions,
//!   history encrypted at rest, outbox).
//! - `payload`: the plaintext inside Olm messages.
//! - `secrets`: the keychain-backed pickle and chat keys.
//! - `model`: the types commands and events hand to the UI.
//!
//! All crypto runs here, in Rust. The WebView only receives decrypted messages for display.

pub mod crypto;
pub mod model;
pub mod payload;
pub mod secrets;
pub mod store;
