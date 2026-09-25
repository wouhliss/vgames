//! Server settings stored in `server_settings` (registration mode, name, MOTD).
//!
//! `server.name` is optional: when an owner has not renamed the server, the name comes
//! from `VGAMES_SERVER_NAME`.

use sqlx::PgPool;
use time::OffsetDateTime;
use vgames_proto::discovery::RegistrationMode;

use crate::error::ApiResult;

pub const REGISTRATION_MODE: &str = "registration.mode";
pub const MOTD: &str = "server.motd";
pub const NAME: &str = "server.name";
pub const KEYS: [&str; 3] = [REGISTRATION_MODE, MOTD, NAME];

#[derive(Clone, Debug)]
pub struct Settings {
    pub registration_mode: RegistrationMode,
    pub motd: Option<String>,
    /// Set by an owner in the admin UI; `None` means `VGAMES_SERVER_NAME`.
    pub name: Option<String>,
    /// Latest change of any setting (the admin ETag).
    pub updated_at: OffsetDateTime,
}

impl Settings {
    pub fn name_or<'a>(&'a self, configured: &'a str) -> &'a str {
        self.name.as_deref().unwrap_or(configured)
    }
}

/// Loads all settings. Unknown or malformed values fall back to the safest default
/// (`allowlist`, empty MOTD, configured name) and are logged.
pub async fn load(db: &PgPool) -> ApiResult<Settings> {
    let rows = sqlx::query!(
        r#"SELECT key, value, updated_at FROM server_settings WHERE key = ANY($1)"#,
        &KEYS.map(str::to_string)[..]
    )
    .fetch_all(db)
    .await?;
    let mut s = Settings {
        registration_mode: RegistrationMode::Allowlist,
        motd: None,
        name: None,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    };
    for row in rows {
        s.updated_at = s.updated_at.max(row.updated_at);
        match row.key.as_str() {
            REGISTRATION_MODE => match serde_json::from_value::<RegistrationMode>(row.value) {
                Ok(m) => s.registration_mode = m,
                Err(e) => {
                    tracing::warn!(error = %e, "invalid registration.mode setting; using allowlist")
                }
            },
            MOTD => {
                s.motd = row
                    .value
                    .as_str()
                    .filter(|m| !m.is_empty())
                    .map(str::to_string)
            }
            NAME => {
                s.name = row
                    .value
                    .as_str()
                    .filter(|n| !n.is_empty())
                    .map(str::to_string)
            }
            _ => {}
        }
    }
    Ok(s)
}
