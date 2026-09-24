//! Postgres-backed job queue (A1-T06).

use crate::state::AppState;

/// Runs background workers until shutdown. Filled in by A1-T06.
pub async fn run_workers(state: AppState) {
    state.shutdown.cancelled().await;
}
