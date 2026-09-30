//! Per-install bookkeeping that is not part of the install lifecycle itself.

use crate::events::PackageRef;

use super::{Db, DbError};

/// Adds a finished play session to `installs.playtime_seconds` and sets
/// `last_played_at`. Returns `false` when the package is not registered
/// (for example, uninstalled while it ran).
pub async fn record_playtime(
    db: &Db,
    package: PackageRef,
    seconds: u32,
    ended_at: i64,
) -> Result<bool, DbError> {
    db.call(move |conn| {
        let changed = conn.execute(
            "UPDATE installs
                SET playtime_seconds = playtime_seconds + ?3,
                    last_played_at = max(coalesce(last_played_at, 0), ?4)
              WHERE server_id = ?1 AND package_id = ?2",
            rusqlite::params![
                package.server_id.to_string(),
                package.package_id.to_string(),
                seconds,
                ended_at
            ],
        )?;
        Ok(changed == 1)
    })
    .await
}

/// `(playtime_seconds, last_played_at)` of an install.
pub async fn playtime(db: &Db, package: PackageRef) -> Result<Option<(i64, Option<i64>)>, DbError> {
    db.call(move |conn| {
        let mut statement = conn.prepare(
            "SELECT playtime_seconds, last_played_at FROM installs
              WHERE server_id = ?1 AND package_id = ?2",
        )?;
        let mut rows = statement.query([
            package.server_id.to_string(),
            package.package_id.to_string(),
        ])?;
        match rows.next()? {
            Some(row) => Ok(Some((row.get(0)?, row.get(1)?))),
            None => Ok(None),
        }
    })
    .await
}

/// Every registered install with its directory (`<library path>/<dir_name>`).
pub async fn roots(db: &Db) -> Result<Vec<(PackageRef, std::path::PathBuf)>, DbError> {
    db.call(|conn| {
        let mut statement = conn.prepare(
            "SELECT i.server_id, i.package_id, l.path, i.dir_name
               FROM installs i JOIN libraries l ON l.id = i.library_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut roots = Vec::new();
        for row in rows {
            let (server, package, library, dir) = row?;
            // Rows with ids that are not UUIDs cannot belong to a real install.
            if let (Ok(server_id), Ok(package_id)) = (
                uuid::Uuid::parse_str(&server),
                uuid::Uuid::parse_str(&package),
            ) {
                let package = PackageRef {
                    server_id,
                    package_id,
                };
                roots.push((package, std::path::Path::new(&library).join(dir)));
            }
        }
        Ok(roots)
    })
    .await
}

/// The install directory and its name inside the library, if registered.
pub async fn find(
    db: &Db,
    package: PackageRef,
) -> Result<Option<(std::path::PathBuf, String)>, DbError> {
    db.call(move |conn| {
        let mut statement = conn.prepare(
            "SELECT l.path, i.dir_name
               FROM installs i JOIN libraries l ON l.id = i.library_id
              WHERE i.server_id = ?1 AND i.package_id = ?2",
        )?;
        let mut rows = statement.query([
            package.server_id.to_string(),
            package.package_id.to_string(),
        ])?;
        match rows.next()? {
            Some(row) => {
                let library: String = row.get(0)?;
                let dir: String = row.get(1)?;
                Ok(Some((std::path::Path::new(&library).join(&dir), dir)))
            }
            None => Ok(None),
        }
    })
    .await
}

/// What a launch needs from an install row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallRow {
    pub library_path: std::path::PathBuf,
    pub root: std::path::PathBuf,
    /// `installs.state` (`installed`, `installing`, …).
    pub state: String,
    pub version_id: String,
    pub sequence: i64,
    pub platform: String,
}

pub async fn row(db: &Db, package: PackageRef) -> Result<Option<InstallRow>, DbError> {
    db.call(move |conn| {
        let mut statement = conn.prepare(
            "SELECT l.path, i.dir_name, i.state, i.version_id, i.sequence, i.platform
               FROM installs i JOIN libraries l ON l.id = i.library_id
              WHERE i.server_id = ?1 AND i.package_id = ?2",
        )?;
        let mut rows = statement.query([
            package.server_id.to_string(),
            package.package_id.to_string(),
        ])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let library: String = row.get(0)?;
        let dir: String = row.get(1)?;
        let library_path = std::path::PathBuf::from(library);
        Ok(Some(InstallRow {
            root: library_path.join(dir),
            library_path,
            state: row.get(2)?,
            version_id: row.get(3)?,
            sequence: row.get(4)?,
            platform: row.get(5)?,
        }))
    })
    .await
}

/// Registers a new install in state `installing` (no-op when the package
/// already has a row, e.g. when resuming).
#[allow(clippy::too_many_arguments)]
pub async fn begin(
    db: &Db,
    package: PackageRef,
    library_id: uuid::Uuid,
    dir_name: String,
    version_id: uuid::Uuid,
    sequence: i64,
    platform: String,
) -> Result<(), DbError> {
    db.call(move |conn| {
        conn.execute(
            "INSERT OR IGNORE INTO installs (server_id, package_id, library_id, dir_name,
             version_id, sequence, platform, state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'installing')",
            rusqlite::params![
                package.server_id.to_string(),
                package.package_id.to_string(),
                library_id.to_string(),
                dir_name,
                version_id.to_string(),
                sequence,
                platform
            ],
        )?;
        Ok(())
    })
    .await
}

/// Marks a finished install playable.
pub async fn mark_installed(db: &Db, package: PackageRef, size_bytes: u64) -> Result<(), DbError> {
    let size = i64::try_from(size_bytes).unwrap_or(i64::MAX);
    db.call(move |conn| {
        conn.execute(
            "UPDATE installs SET state = 'installed', installed_at = ?3, size_bytes = ?4
             WHERE server_id = ?1 AND package_id = ?2",
            rusqlite::params![
                package.server_id.to_string(),
                package.package_id.to_string(),
                super::now_unix(),
                size
            ],
        )?;
        Ok(())
    })
    .await
}

/// Deletes the row of an install that never completed (state `installing`).
/// Returns its directory name when a row was removed.
pub async fn delete_unfinished(db: &Db, package: PackageRef) -> Result<Option<String>, DbError> {
    db.call(move |conn| {
        let ids = rusqlite::params![package.server_id.to_string(), package.package_id.to_string()];
        let dir: Option<String> = conn
            .query_row(
                "SELECT dir_name FROM installs
                 WHERE server_id = ?1 AND package_id = ?2 AND state = 'installing'",
                ids,
                |row| row.get(0),
            )
            .ok();
        if dir.is_some() {
            conn.execute(
                "DELETE FROM installs WHERE server_id = ?1 AND package_id = ?2 AND state = 'installing'",
                ids,
            )?;
        }
        Ok(dir)
    })
    .await
}
