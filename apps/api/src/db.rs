//! Database pool and migrations.

use std::time::Duration;

use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};

use crate::config::Config;

/// Embedded migrations from `apps/api/migrations`.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Session settings of every pool connection.
///
/// `plan_cache_mode = force_custom_plan`: list queries take their optional filters as
/// `($n IS NULL OR col = $n)` so they stay compile-time checked. After five executions Postgres
/// may switch a prepared statement to a generic plan, which cannot fold those conditions away:
/// the catalog then sorts every published package and a filtered audit page walks the whole log
/// (35–160 ms with 100k packages and 1M audit rows). Custom plans use the indexes (under 1 ms,
/// 25 ms at worst) for about 0.2 ms of planning per statement. `apps/api/bench/explain.sql`
/// shows both.
pub const SESSION_OPTIONS: [(&str, &str); 1] = [("plan_cache_mode", "force_custom_plan")];

/// Connection options for the API pool: `url` plus [`SESSION_OPTIONS`].
pub fn connect_options(url: &str) -> Result<PgConnectOptions, sqlx::Error> {
    Ok(url.parse::<PgConnectOptions>()?.options(SESSION_OPTIONS))
}

pub async fn connect(config: &Config) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(config.db_max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(connect_options(config.database_url.expose())?)
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
