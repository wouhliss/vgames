//! Test support (feature `testkit`, never in release builds): signed test
//! packages built with `vgames-pack`, a loopback storage server that serves
//! packs with `Range` support and injects faults, and a mock API.
//!
//! Panicking on setup errors is the point here, so the usual lints are off.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

pub mod api;
pub mod package;
pub mod rig;
pub mod upload;

pub use api::MockApi;
pub use package::{
    Content, FileSpec, Identity, TestPackage, default_expected, empty_dirs, install_mode,
    random_files, read_tree, trust_state, write_tree,
};
pub use rig::{Fault, Links, Rig};
pub use upload::{MockPublishApi, UploadFault, UploadRig};
