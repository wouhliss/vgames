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

/// The install finished: playable from now on.
pub async fn set_installed(
    db: &Db,
    package: PackageRef,
    version_id: uuid::Uuid,
    sequence: i64,
    size_bytes: u64,
) -> Result<(), DbError> {
    db.call(move |conn| {
        conn.execute(
            "UPDATE installs SET state = 'installed', version_id = ?3, sequence = ?4,
                    size_bytes = ?5, installed_at = coalesce(installed_at, ?6)
              WHERE server_id = ?1 AND package_id = ?2",
            rusqlite::params![
                package.server_id.to_string(),
                package.package_id.to_string(),
                version_id.to_string(),
                sequence,
                i64::try_from(size_bytes).unwrap_or(i64::MAX),
                super::now_unix(),
            ],
        )?;
        Ok(())
    })
    .await
}

/// Forgets an install (cancelled with delete, uninstalled).
pub async fn delete(db: &Db, package: PackageRef) -> Result<(), DbError> {
    db.call(move |conn| {
        conn.execute(
            "DELETE FROM installs WHERE server_id = ?1 AND package_id = ?2",
            [
                package.server_id.to_string(),
                package.package_id.to_string(),
            ],
        )?;
        Ok(())
    })
    .await
}

/// Catalog data recorded when the install started.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CatalogInfo {
    pub title: String,
    pub slug: String,
    pub version_label: String,
    pub cover_asset_id: Option<uuid::Uuid>,
}

pub async fn catalog_info(db: &Db, package: PackageRef) -> Result<Option<CatalogInfo>, DbError> {
    db.call(move |conn| {
        let mut statement = conn.prepare(
            "SELECT title, slug, version_label, cover_asset_id FROM installs
              WHERE server_id = ?1 AND package_id = ?2",
        )?;
        let mut rows = statement.query([
            package.server_id.to_string(),
            package.package_id.to_string(),
        ])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let cover: Option<String> = row.get(3)?;
        Ok(Some(CatalogInfo {
            title: row.get(0)?,
            slug: row.get(1)?,
            version_label: row.get(2)?,
            cover_asset_id: cover.and_then(|c| uuid::Uuid::parse_str(&c).ok()),
        }))
    })
    .await
}
