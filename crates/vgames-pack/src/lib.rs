//! # vgames-pack
//!
//! Turns a folder into the `vgames.manifest/1` layout (02-package-format §4):
//! files → chunks (≤ 4 MiB) → packs (≤ 256 MiB), hashes everything, and builds
//! the manifest bytes. Pure planning and hashing over a [`source::SourceReader`],
//! so the same code runs natively (CLI, launcher admin mode) and in a browser
//! Web Worker (admin-web, feature `wasm`).
//!
//! Pack bytes are generated on the fly from the source files while uploading
//! ([`source::PackSource`]); no pack file is ever written to disk.
//!
//! Typical native flow:
//! 1. [`scan::scan`] a folder, then [`plan::Plan::new`] (sorts, validates paths, lays out chunks).
//! 2. [`plan::Plan::raw_packing`] (or [`source::analyze`] for `compression = auto`).
//! 3. Stream every pack from [`source::PackSource::open_pack`]; hashes are collected on the way.
//! 4. [`manifest::build`] with the collected [`source::Hashes`], then sign (vgames-core).
//!
//! [`verify::PackStreamVerifier`] checks pack bytes against a manifest.
//!
//! Owner: Agent 2 (Tauri & Systems). Spec: `docs/architecture/02-package-format.md`.

pub mod encode;
pub mod hash;
pub mod manifest;
pub mod plan;
#[cfg(not(target_arch = "wasm32"))]
pub mod scan;
pub mod source;
pub mod verify;
#[cfg(feature = "wasm")]
pub mod wasm;

pub use encode::{Compression, Encoding};
pub use plan::{Packing, Plan, SourceFile};
pub use source::{HashCollector, Hashes, PackSource, SourceReader};
pub use verify::{PackExpectation, PackStreamVerifier};
pub use vgames_core::layout::{CHUNK_SIZE, PACK_SIZE};
/// The single chunk/pack layout function and path rules live in `vgames-core`
/// (re-exported so packer callers need one import path).
pub use vgames_core::{layout, paths};
