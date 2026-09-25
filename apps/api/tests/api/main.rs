//! API integration tests, compiled as one binary: one link instead of one per file keeps
//! CI runners and developer disks from filling up (each binary carries the whole dependency tree).
//! The OpenAPI drift test stays a separate binary (`cargo xtask openapi check` runs it by name).

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
