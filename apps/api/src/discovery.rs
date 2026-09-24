//! `/.well-known/vgames.json` and `/v1/health`.

use std::time::Duration;

use axum::{extract::State, http::StatusCode, response::IntoResponse};
use utoipa_axum::{router::OpenApiRouter, routes};
use vgames_proto::discovery::{
    ApiVersion, DependencyStatus, Feature, Health, HealthStatus, ServerInfo,
};

use crate::{error::ApiResult, http::json::JsonResponse, state::AppState};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(server_info))
        .routes(routes!(health))
}

/// Server identity and capabilities
#[utoipa::path(
    get,
    path = "/.well-known/vgames.json",
    tag = "discovery",
    operation_id = "getServerInfo",
    security(()),
    responses((status = 200, description = "Server information", body = ServerInfo))
)]
pub async fn server_info(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let settings = crate::settings::load(&state.db).await?;
    let mut features = vec![
        Feature::CloudSaves,
        Feature::Social,
        Feature::Messaging,
        Feature::Invites,
    ];
    if state.config.admin_dist.is_some() {
        features.push(Feature::AdminWeb);
    }
    let info = ServerInfo {
        format: "vgames.server/1".to_string(),
        server_id: state.config.server_id,
        name: state.config.server_name.clone(),
        motd: settings.motd.filter(|m| !m.is_empty()),
        api_versions: vec![ApiVersion::V1],
        root_public_key: state.config.root_public_key.to_base64(),
        root_key_fingerprint: state.config.root_public_key.fingerprint().to_string(),
        registration_mode: Some(settings.registration_mode),
        features,
        min_launcher_version: None,
    };
    Ok((
        [(axum::http::header::CACHE_CONTROL, "public, max-age=300")],
        axum::Json(info),
    ))
}

/// Liveness and dependency readiness
#[utoipa::path(
    get,
    path = "/v1/health",
    tag = "discovery",
    operation_id = "getHealth",
    security(()),
    responses(
        (status = 200, description = "Healthy", body = Health),
        (status = 503, description = "A dependency is unavailable", body = Health)
    )
)]
pub async fn health(State(state): State<AppState>) -> JsonResponse<Health> {
    let db_ok = matches!(
        tokio::time::timeout(
            Duration::from_secs(2),
            sqlx::query_scalar!(r#"SELECT 1 AS "one!""#).fetch_one(&state.db)
        )
        .await,
        Ok(Ok(_))
    );
    let storage_ok = matches!(
        tokio::time::timeout(Duration::from_secs(2), state.storage.ping()).await,
        Ok(true)
    );
    let dep = |ok: bool| {
        if ok {
            DependencyStatus::Ok
        } else {
            DependencyStatus::Down
        }
    };
    let (status, code) = match (db_ok, storage_ok) {
        (true, true) => (HealthStatus::Ok, StatusCode::OK),
        (true, false) => (HealthStatus::Degraded, StatusCode::SERVICE_UNAVAILABLE),
        _ => (HealthStatus::Down, StatusCode::SERVICE_UNAVAILABLE),
    };
    JsonResponse(
        code,
        Health {
            status,
            db: Some(dep(db_ok)),
            storage: Some(dep(storage_ok)),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        },
    )
}
