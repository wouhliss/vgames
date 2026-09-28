//! Desktop shortcuts the launcher created, so uninstall can remove them.

use std::path::PathBuf;

use crate::events::PackageRef;

use super::{Db, DbError, now_unix};

pub async fn add(db: &Db, package: PackageRef, path: PathBuf) -> Result<(), DbError> {
    let path = path.to_string_lossy().into_owned();
    db.call(move |conn| {
        conn.execute(
            "INSERT OR REPLACE INTO shortcuts (server_id, package_id, path, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                package.server_id.to_string(),
                package.package_id.to_string(),
                path,
                now_unix()
            ],
        )?;
        Ok(())
    })
    .await
}

pub async fn for_package(db: &Db, package: PackageRef) -> Result<Vec<PathBuf>, DbError> {
    db.call(move |conn| {
        let mut statement = conn.prepare(
            "SELECT path FROM shortcuts WHERE server_id = ?1 AND package_id = ?2 ORDER BY created_at",
        )?;
        let rows = statement.query_map(
            [package.server_id.to_string(), package.package_id.to_string()],
            |row| row.get::<_, String>(0),
        )?;
        Ok(rows
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(PathBuf::from)
            .collect())
    })
    .await
}

pub async fn forget(db: &Db, package: PackageRef) -> Result<(), DbError> {
    db.call(move |conn| {
        conn.execute(
            "DELETE FROM shortcuts WHERE server_id = ?1 AND package_id = ?2",
            [
                package.server_id.to_string(),
                package.package_id.to_string(),
            ],
        )?;
        Ok(())
    })
    .await
}
