//! # vgames-transfer
//!
//! - [`download`]: plans byte ranges over packs, streams them over N adaptive
//!   connections, verifies every chunk's BLAKE3 in memory, and writes the bytes
//!   straight into their final files with positional writes. Disk usage never
//!   exceeds the installed size. Crash-safe resume via a chunk journal.
//! - [`install`]: fresh installs on top of the engine (checks, preallocation,
//!   `install.json`, finalize), and removal without following links.
//! - `upload`: streams packs generated on the fly by `vgames-pack` into GCS
//!   resumable sessions, several packs in parallel.
//! - `update`: diffs two manifests and reuses local chunks.
//!
//! Owner: Agent 2 (Tauri & Systems). Spec: `docs/architecture/02-package-format.md`.

pub mod download;
pub mod fsutil;
pub mod http;
pub mod install;
pub mod sys;
#[cfg(feature = "testkit")]
pub mod testkit;
pub mod upload;
