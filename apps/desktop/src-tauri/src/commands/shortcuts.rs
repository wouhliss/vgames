//! Desktop shortcut commands (A2-T10).

use tauri::State;

use crate::db;
use crate::error::AppError;
use crate::events::PackageRef;
use crate::shortcuts::{self, ShortcutFormat};
use crate::state::AppState;

/// Creates a desktop shortcut that opens `vgames://launch/<package id>`.
/// Returns the shortcut's path.
// `pkg`, not `package`: that is a reserved word in strict-mode TypeScript.
#[tauri::command]
#[specta::specta]
pub async fn shortcut_create(
    state: State<'_, AppState>,
    pkg: PackageRef,
) -> Result<String, AppError> {
    let package = pkg;
    let (_, dir_name) = db::installs::find(&state.db, package)
        .await
        .map_err(|e| AppError::internal("read the install", &e))?
        .ok_or(AppError::NotFound)?;
    let desktop = shortcuts::desktop_dir().ok_or(AppError::NotFound)?;
    let package_id = package.package_id;
    let created = tokio::task::spawn_blocking(move || {
        // The install folder is named after the title (a slug); the catalog
        // title replaces it once the catalog cache is available (A2-T08).
        shortcuts::create(
            &desktop,
            ShortcutFormat::native(),
            &dir_name,
            package_id,
            None,
        )
    })
    .await
    .map_err(|e| AppError::internal("create the shortcut", &e))?;
    let path = match created {
        Ok(path) => path,
        Err(shortcuts::ShortcutError::Io { path, source, .. }) => {
            tracing::warn!(%source, "cannot create a desktop shortcut");
            return Err(AppError::Io {
                path: Some(path.to_string_lossy().into_owned()),
                detail: "Cannot write the shortcut".into(),
            });
        }
        Err(error) => {
            return Err(AppError::internal("create the shortcut", &error));
        }
    };
    db::shortcuts::add(&state.db, package, path.clone())
        .await
        .map_err(|e| AppError::internal("record the shortcut", &e))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Removes every shortcut created for `package` (uninstall). Shortcuts the
/// user changed or replaced are left in place.
pub async fn remove_for_package(db: &db::Db, package: PackageRef) -> Result<(), db::DbError> {
    let paths = db::shortcuts::for_package(db, package).await?;
    let package_id = package.package_id;
    let removed = tokio::task::spawn_blocking(move || {
        for path in paths {
            if let Err(error) = shortcuts::remove(&path, package_id) {
                tracing::warn!(error = %crate::error::DisplayChain(&error), "cannot remove a shortcut");
            }
        }
    })
    .await;
    if let Err(error) = removed {
        tracing::warn!(%error, "cannot remove shortcuts");
    }
    db::shortcuts::forget(db, package).await
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    async fn uninstall_removes_recorded_shortcuts_but_not_replaced_ones() {
        let package = PackageRef {
            server_id: Uuid::from_u128(1),
            package_id: Uuid::from_u128(2),
        };
        let db = db::Db::open_in_memory().unwrap();
        db.call(move |conn| {
            conn.execute(
                "INSERT INTO servers (id, url, name, root_public_key, root_fingerprint, added_at)
                 VALUES (?1, 'https://example.test', 'Test', zeroblob(32), 'VG1', 0)",
                [package.server_id.to_string()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let desktop = tempfile::tempdir().unwrap();
        let format = ShortcutFormat::native();
        let ours =
            shortcuts::create(desktop.path(), format, "Game", package.package_id, None).unwrap();
        let replaced =
            shortcuts::create(desktop.path(), format, "Game", package.package_id, None).unwrap();
        std::fs::write(&replaced, b"the player's own file").unwrap();
        db::shortcuts::add(&db, package, ours.clone())
            .await
            .unwrap();
        db::shortcuts::add(&db, package, replaced.clone())
            .await
            .unwrap();
        assert_eq!(
            db::shortcuts::for_package(&db, package)
                .await
                .unwrap()
                .len(),
            2
        );

        remove_for_package(&db, package).await.unwrap();
        assert!(!ours.exists());
        assert!(replaced.exists());
        assert!(
            db::shortcuts::for_package(&db, package)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
