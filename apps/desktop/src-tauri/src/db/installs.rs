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
