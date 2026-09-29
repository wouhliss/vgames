//! Social features in the launcher core (docs/architecture/05-social.md). Owner: Agent 4.
//!
//! - `crypto`: Olm (vodozemac) accounts, sessions, signatures, safety numbers, body cipher.
//! - `store`: the local E2EE state and message engine over SQLite (pins, sessions,
//!   history encrypted at rest, outbox).
//! - `payload`: the plaintext inside Olm messages.
//! - `secrets`: the keychain-backed pickle and chat keys.
//! - `model`: the types commands and events hand to the UI.
//! - `ports`: the session slot and OS idle time; `session_bridge` fills the slot from
//!   Agent 2's server sessions and refreshes refused tokens.
//! - `api`, `realtime`: REST calls and the realtime socket to the active server.
//! - `service`, `presence`, `commands`: friends, blocks, settings, presence publishing,
//!   and the Tauri commands and events over them.
//!
//! All crypto runs here, in Rust. The WebView only receives decrypted messages for display.

pub mod api;
pub mod commands;
pub mod crypto;
mod idle;
pub mod model;
mod netwatch;
pub mod payload;
pub mod ports;
pub mod presence;
pub mod realtime;
pub mod secrets;
pub mod service;
pub mod session_bridge;
pub mod store;
