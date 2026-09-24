//! Process lifecycle: serve HTTP, run workers, shut down gracefully.

use std::{net::SocketAddr, time::Duration};

use tokio::net::TcpListener;

use crate::{Config, state::AppState};

/// Which parts of the service this process runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Role {
    All,
    Api,
    Worker,
}

/// How long in-flight requests may finish after a shutdown signal.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

pub async fn run(config: Config, role: Role) -> anyhow::Result<()> {
    let db = crate::db::connect(&config).await?;
    let state = AppState::new(config, db)?;

    let shutdown = state.shutdown.clone();
    tokio::spawn(async move {
        wait_for_signal().await;
        tracing::info!("shutdown signal received; draining");
        shutdown.cancel();
    });

    let workers = if role != Role::Api {
        Some(tokio::spawn(crate::jobs::run_workers(state.clone())))
    } else {
        None
    };

    if role != Role::Worker {
        serve(state.clone()).await?;
    } else {
        state.shutdown.cancelled().await;
    }

    if let Some(w) = workers {
        let _ = tokio::time::timeout(DRAIN_TIMEOUT, w).await;
    }
    state.db.close().await;
    tracing::info!("stopped");
    Ok(())
}

/// Serves until `state.shutdown` is cancelled, then drains for at most [`DRAIN_TIMEOUT`].
pub async fn serve(state: AppState) -> anyhow::Result<()> {
    let listener = TcpListener::bind(state.config.bind_addr).await?;
    tracing::info!(addr = %state.config.bind_addr, origin = %state.config.public_origin(), "listening");
    serve_on(listener, state).await
}

pub async fn serve_on(listener: TcpListener, state: AppState) -> anyhow::Result<()> {
    let shutdown = state.shutdown.clone();
    let app = crate::http::router(state).into_make_service_with_connect_info::<SocketAddr>();
    let server = axum::serve(listener, app).with_graceful_shutdown({
        let s = shutdown.clone();
        async move { s.cancelled().await }
    });
    let mut server = std::pin::pin!(server.into_future());
    tokio::select! {
        res = &mut server => res?,
        _ = async { shutdown.cancelled().await; tokio::time::sleep(DRAIN_TIMEOUT).await } => {
            tracing::warn!("drain timeout reached; closing remaining connections");
        }
    }
    Ok(())
}

async fn wait_for_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
}
