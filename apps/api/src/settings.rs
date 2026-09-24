//! Server settings stored in `server_settings` (registration mode, MOTD).

use sqlx::PgPool;
use vgames_proto::discovery::RegistrationMode;

use crate::error::ApiResult;

#[derive(Clone, Debug)]
pub struct Settings {
    pub registration_mode: RegistrationMode,
    pub motd: Option<String>,
}

/// Loads all settings. Unknown or malformed values fall back to the safest default
/// (`allowlist`, empty MOTD) and are logged.
pub async fn load(db: &PgPool) -> ApiResult<Settings> {
    let rows = sqlx::query!(
        r#"SELECT key, value FROM server_settings WHERE key IN ('registration.mode', 'server.motd')"#
    )
    .fetch_all(db)
    .await?;
    let mut s = Settings {
        registration_mode: RegistrationMode::Allowlist,
        motd: None,
    };
    for row in rows {
        match row.key.as_str() {
            "registration.mode" => match serde_json::from_value::<RegistrationMode>(row.value) {
                Ok(m) => s.registration_mode = m,
                Err(e) => {
                    tracing::warn!(error = %e, "invalid registration.mode setting; using allowlist")
                }
            },
            "server.motd" => s.motd = row.value.as_str().map(str::to_string),
            _ => {}
        }
    }
    Ok(s)
}
