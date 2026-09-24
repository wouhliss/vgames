//! `/.well-known/vgames.json` and `/v1/health`.

use std::time::Duration;

use axum::{extract::State, http::StatusCode, response::IntoResponse};
use base64::Engine as _;
use utoipa_axum::{router::OpenApiRouter, routes};
use vgames_proto::discovery::{DependencyStatus, Health, HealthStatus, ServerInfo};

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
    let fingerprint = crate::trust::root_fingerprint(&state.config.root_public_key)?;
    let mut features = vec!["cloud_saves", "social", "messaging", "invites"];
    if state.config.admin_dist.is_some() {
        features.push("admin_web");
    }
    let info = ServerInfo {
        format: "vgames.server/1".to_string(),
        server_id: state.config.server_id,
        name: state.config.server_name.clone(),
        motd: settings.motd.filter(|m| !m.is_empty()),
        api_versions: vec!["v1".to_string()],
        root_public_key: base64::engine::general_purpose::STANDARD
            .encode(state.config.root_public_key),
        root_key_fingerprint: fingerprint,
        registration_mode: Some(settings.registration_mode),
        features: features.into_iter().map(String::from).collect(),
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
    let db = if db_ok {
        DependencyStatus::Ok
    } else {
        DependencyStatus::Down
    };
    let status = if db_ok {
        HealthStatus::Ok
    } else {
        HealthStatus::Down
    };
    let code = if db_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    JsonResponse(
        code,
        Health {
            status,
            db: Some(db),
            storage: None,
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        },
    )
}
