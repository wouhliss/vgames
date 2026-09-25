//! Social features in the launcher core (docs/architecture/05-social.md). Owner: Agent 4.
//!
//! - `crypto`: Olm (vodozemac) accounts, sessions, signatures, safety numbers, body cipher.
//!
//! All crypto runs here, in Rust. The WebView only receives decrypted messages for display.

pub mod crypto;
