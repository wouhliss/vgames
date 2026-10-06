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

/// One install of a server as the library lists it (INS-04).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedInstall {
    pub package: PackageRef,
    pub library_id: uuid::Uuid,
    pub library_path: std::path::PathBuf,
    pub root: std::path::PathBuf,
    pub version_id: String,
    pub sequence: i64,
    pub platform: String,
    /// `installs.state`.
    pub state: String,
    pub installed_at: Option<i64>,
    pub last_played_at: Option<i64>,
    pub playtime_seconds: i64,
    pub size_bytes: i64,
    pub catalog: CatalogInfo,
}

/// Every install of `server_id`, by title.
pub async fn list(db: &Db, server_id: uuid::Uuid) -> Result<Vec<ListedInstall>, DbError> {
    db.call(move |conn| {
        let mut statement = conn.prepare(
            "SELECT i.package_id, i.library_id, l.path, i.dir_name, i.version_id, i.sequence,
                    i.platform, i.state, i.installed_at, i.last_played_at, i.playtime_seconds,
                    i.size_bytes, i.title, i.slug, i.version_label, i.cover_asset_id
               FROM installs i JOIN libraries l ON l.id = i.library_id
              WHERE i.server_id = ?1
              ORDER BY i.title COLLATE NOCASE, i.package_id",
        )?;
        let rows = statement.query_map([server_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<i64>>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, i64>(11)?,
                (
                    row.get::<_, String>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, Option<String>>(15)?,
                ),
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (
                package_id,
                library_id,
                library,
                dir,
                version_id,
                sequence,
                platform,
                state,
                installed_at,
                last_played_at,
                playtime_seconds,
                size_bytes,
                (title, slug, version_label, cover),
            ) = row?;
            // Rows this build cannot read are skipped, not fatal.
            let (Ok(package_id), Ok(library_id)) = (
                uuid::Uuid::parse_str(&package_id),
                uuid::Uuid::parse_str(&library_id),
            ) else {
                tracing::warn!("skipping an install row with an invalid id");
                continue;
            };
            let library_path = std::path::PathBuf::from(library);
            out.push(ListedInstall {
                package: PackageRef {
                    server_id,
                    package_id,
                },
                library_id,
                root: library_path.join(&dir),
                library_path,
                version_id,
                sequence,
                platform,
                state,
                installed_at,
                last_played_at,
                playtime_seconds,
                size_bytes,
                catalog: CatalogInfo {
                    title,
                    slug,
                    version_label,
                    cover_asset_id: cover.and_then(|c| uuid::Uuid::parse_str(&c).ok()),
                },
            });
        }
        Ok(out)
    })
    .await
}

/// Records what an install is busy with (`installed`, `updating`,
/// `repairing`, `moving`, `uninstalling`); the launcher refuses to start a
/// game that is not `installed`.
pub async fn set_state(db: &Db, package: PackageRef, state: &'static str) -> Result<(), DbError> {
    db.call(move |conn| {
        conn.execute(
            "UPDATE installs SET state = ?3 WHERE server_id = ?1 AND package_id = ?2",
            [
                package.server_id.to_string(),
                package.package_id.to_string(),
                state.to_owned(),
            ],
        )?;
        Ok(())
    })
    .await
}

/// The label of the installed version (after an update).
pub async fn set_version_label(db: &Db, package: PackageRef, label: String) -> Result<(), DbError> {
    db.call(move |conn| {
        conn.execute(
            "UPDATE installs SET version_label = ?3 WHERE server_id = ?1 AND package_id = ?2",
            [
                package.server_id.to_string(),
                package.package_id.to_string(),
                label,
            ],
        )?;
        Ok(())
    })
    .await
}

/// Records a moved install's new library and folder.
pub async fn set_location(
    db: &Db,
    package: PackageRef,
    library_id: uuid::Uuid,
    dir_name: String,
) -> Result<(), DbError> {
    db.call(move |conn| {
        conn.execute(
            "UPDATE installs SET library_id = ?3, dir_name = ?4 WHERE server_id = ?1 AND package_id = ?2",
            [
                package.server_id.to_string(),
                package.package_id.to_string(),
                library_id.to_string(),
                dir_name,
            ],
        )?;
        Ok(())
    })
    .await
}
