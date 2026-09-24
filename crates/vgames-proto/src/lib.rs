//! # vgames-proto
//!
//! Request/response bodies for `/v1` and the realtime event envelope, mirroring
//! `openapi/openapi.yaml`. The API derives its OpenAPI schemas from these types
//! (feature `openapi`); the launcher deserializes with the same structs.
//!
//! Owners: Agent 1 (all modules except `social`), Agent 4 (`social`, `realtime`).
//! Spec: `docs/architecture/03-api.md`.
//!
//! Conventions: snake_case fields, RFC 3339 UTC timestamps, UUIDv7 ids. Request
//! bodies reject unknown fields (`deny_unknown_fields`); optional response fields
//! are omitted when absent.

pub mod auth;
pub mod common;
pub mod discovery;

pub use common::{FieldError, Problem};
