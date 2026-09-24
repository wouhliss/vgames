//! vgames-api: REST `/v1`, realtime gateway, job runner and admin UI host.
//! Owner: Agent 1 (Backend & DB); `social` belongs to Agent 4.

pub mod admin;
pub mod assets;
pub mod auth;
pub mod config;
pub mod db;
pub mod discovery;
pub mod error;
pub mod http;
pub mod jobs;
pub mod metadata;
pub mod openapi;
pub mod openapi_problems;
pub mod packages;
pub mod realtime;
pub mod saves;
pub mod secret;
pub mod server;
pub mod settings;
pub mod social;
pub mod state;
pub mod storage;
pub mod telemetry;
pub mod trust;
pub mod uploads;
pub mod users;
pub mod versions;

pub use config::Config;
pub use state::AppState;
