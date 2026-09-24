//! Database pool and migrations.

use std::time::Duration;

use sqlx::{PgPool, postgres::PgPoolOptions};

use crate::config::Config;

/// Embedded migrations from `apps/api/migrations`.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(config: &Config) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(config.db_max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .connect(config.database_url.expose())
        .await
}

/// Applies pending migrations using the migration (owner) role.
pub async fn migrate(config: &Config) -> Result<(), sqlx::migrate::MigrateError> {
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(10))
        .connect(config.database_migration_url.expose())
        .await?;
    MIGRATOR.run(&pool).await?;
    pool.close().await;
    Ok(())
}
