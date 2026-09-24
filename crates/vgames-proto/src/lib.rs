//! # vgames-proto
//!
//! Request/response bodies for `/v1` and the realtime event envelope, mirroring
//! `openapi/openapi.yaml`. The API derives its OpenAPI schemas from these types
//! (feature `openapi`); the launcher deserializes with the same structs.
//!
//! Owners: Agent 1 (all modules except `social`), Agent 4 (`social`, `realtime`).
//! Spec: `docs/architecture/03-api.md`.
