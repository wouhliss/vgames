//! API integration tests, built as one test binary.
//!
//! Every separate file under `tests/` becomes its own binary that links the whole server
//! (about 440 MB each with debug info), which filled CI runners' disks. New test files go
//! here as modules. `tests/openapi_contract.rs` stays separate because
//! `cargo xtask openapi check` runs it by name.

mod common;

mod auth;
mod cli;
mod compat;
mod conventions;
mod downloads;
mod finalize;
mod jobs;
mod metadata;
mod packages;
mod realtime;
mod releases;
mod saves;
mod skeleton;
mod storage;
mod trust;
mod versions;
