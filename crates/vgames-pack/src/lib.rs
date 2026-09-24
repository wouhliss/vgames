//! # vgames-pack
//!
//! Turns a folder into the `vgames.manifest/1` layout: files -> extents ->
//! chunks (~4 MiB) -> packs (<= 256 MiB). Pure planning and hashing over a
//! `ReadAt`-style source trait, so the same code runs natively (CLI, launcher
//! admin mode) and in a browser Web Worker (admin-web, feature `wasm`).
//!
//! Pack bytes are *generated on the fly* from the source files while uploading;
//! no pack file is ever written to disk (no 2x footprint on the uploader either).
//!
//! Owner: Agent 2 (Tauri & Systems). Spec: `docs/architecture/02-package-format.md`.
