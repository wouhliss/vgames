//! # vgames-transfer
//!
//! - `download`: plans byte ranges over packs, streams them over N adaptive
//!   connections, verifies every chunk's BLAKE3 in memory, and writes the bytes
//!   straight into their final files with positional writes. Disk usage never
//!   exceeds the installed size. Crash-safe resume via a chunk journal.
//! - `upload`: streams packs generated on the fly by `vgames-pack` into GCS
//!   resumable sessions, several packs in parallel.
//! - `update`: diffs two manifests and reuses local chunks.
//!
//! Owner: Agent 2 (Tauri & Systems). Spec: `docs/architecture/02-package-format.md`.
