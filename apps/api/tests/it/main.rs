//! API integration tests, built as one test binary.
//!
//! Every separate file under `tests/` becomes its own binary that links the whole server
//! (about 440 MB each with debug info), which filled CI runners' disks. New test files go
//! here as modules. `tests/openapi_contract.rs` stays separate because
//! `cargo xtask openapi check` runs it by name.

mod common;

mod admin;
mod admin_web;
mod auth;
mod cli;
mod compat;
mod conventions;
mod downloads;
mod finalize;
mod jobs;
mod limits;
mod metadata;
mod no_panic;
mod packages;
mod realtime;
mod releases;
mod saves;
mod skeleton;
mod social_devices;
mod social_friends;
mod social_presence;
mod social_relay;
mod storage;
mod trust;
mod versions;
