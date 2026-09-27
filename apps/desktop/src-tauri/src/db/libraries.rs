//! SQLite registration of validated library roots. Filesystem checks run on
//! the dedicated DB thread inside the insert transaction, so two concurrent
//! additions cannot create nested libraries from stale path snapshots.

use std::path::{Path, PathBuf};

use rusqlite::params;
use uuid::Uuid;

use super::{Db, DbError, now_unix};
use crate::libraries::{self, LibraryError, LibraryRoot};

#[derive(Debug, thiserror::Error)]
pub enum LibraryStoreError {
    #[error("library label must be between 1 and 100 characters")]
    InvalidLabel,
    #[error("library path cannot be stored as text")]
    NonUnicodePath,
    #[error("stored library ID is invalid")]
    InvalidStoredId,
    #[error("library is not registered")]
    NotFound,
    #[error(transparent)]
    Root(#[from] LibraryError),
    #[error(transparent)]
    Database(#[from] DbError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Library {
    pub root: LibraryRoot,
    pub label: String,
    pub is_default: bool,
    pub created_at: i64,
}

/// Registers one new library. The marker is removed if the SQLite transaction
/// fails; no install or other player file is deleted.
pub async fn add(db: &Db, path: &Path, label: &str) -> Result<Library, LibraryStoreError> {
    let label = label.trim();
    if label.is_empty() || label.chars().count() > 100 || label.chars().any(char::is_control) {
        return Err(LibraryStoreError::InvalidLabel);
    }
    let label = label.to_owned();
    let path = path.to_owned();
    db.call(move |conn| {
        Ok((|| -> Result<Library, LibraryStoreError> {
            let tx = conn.transaction()?;
            let existing = {
                let mut query = tx.prepare("SELECT path FROM libraries")?;
                query
                    .query_map([], |row| row.get::<_, String>(0))?
                    .map(|row| row.map(PathBuf::from))
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            let is_default: bool =
                tx.query_row("SELECT count(*) = 0 FROM libraries", [], |row| row.get(0))?;
            let root = libraries::create_root(&path, &existing)?;
            let canonical = match root.path.to_str() {
                Some(path) => path,
                None => {
                    if let Err(cleanup) = libraries::remove_marker_if_matches(&root) {
                        tracing::warn!(%cleanup, "cannot remove an unregistered library marker");
                    }
                    return Err(LibraryStoreError::NonUnicodePath);
                }
            };
            let created_at = now_unix();
            let result = tx.execute(
                "INSERT INTO libraries (id, path, label, is_default, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    root.id.to_string(),
                    canonical,
                    label,
                    is_default,
                    created_at
                ],
            );
            if let Err(error) = result {
                if let Err(cleanup) = libraries::remove_marker_if_matches(&root) {
                    tracing::warn!(%cleanup, "cannot remove an unregistered library marker");
                }
                return Err(LibraryStoreError::Sqlite(error));
            }
            if let Err(error) = tx.commit() {
                if let Err(cleanup) = libraries::remove_marker_if_matches(&root) {
                    tracing::warn!(%cleanup, "cannot remove an unregistered library marker");
                }
                return Err(error.into());
            }
            Ok(Library {
                root,
                label,
                is_default,
                created_at,
            })
        })())
    })
    .await?
}

pub async fn list(db: &Db) -> Result<Vec<Library>, LibraryStoreError> {
    db.call(|conn| {
        Ok((|| -> Result<Vec<Library>, LibraryStoreError> {
            let mut query = conn.prepare(
                "SELECT id, path, label, is_default, created_at FROM libraries
                 ORDER BY is_default DESC, created_at, id",
            )?;
            let rows = query
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, bool>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter()
                .map(|(id, path, label, is_default, created_at)| {
                    Ok(Library {
                        root: LibraryRoot {
                            id: Uuid::parse_str(&id)
                                .map_err(|_| LibraryStoreError::InvalidStoredId)?,
                            path: PathBuf::from(path),
                        },
                        label,
                        is_default,
                        created_at,
                    })
                })
                .collect()
        })())
    })
    .await?
}

/// Selects a registered library as the default in one transaction.
pub async fn set_default(db: &Db, id: Uuid) -> Result<(), LibraryStoreError> {
    db.call(move |conn| {
        Ok((|| -> Result<(), LibraryStoreError> {
            let tx = conn.transaction()?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM libraries WHERE id = ?1)",
                [id.to_string()],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(LibraryStoreError::NotFound);
            }
            tx.execute(
                "UPDATE libraries SET is_default = 0 WHERE is_default = 1",
                [],
            )?;
            tx.execute(
                "UPDATE libraries SET is_default = 1 WHERE id = ?1",
                [id.to_string()],
            )?;
            tx.commit()?;
            Ok(())
        })())
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree_tempdir() -> tempfile::TempDir {
        tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn adding_is_serialized_and_the_first_library_becomes_default() {
        let dir = worktree_tempdir();
        let first = dir.path().join("one");
        let nested = first.join("nested");
        let second = dir.path().join("two");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(&second).unwrap();
        let db = Db::open_in_memory().unwrap();
        let first = add(&db, &first, " Games ").await.unwrap();
        assert_eq!(first.label, "Games");
        assert!(first.is_default);
        assert!(matches!(
            add(&db, &nested, "Nested").await,
            Err(LibraryStoreError::Root(LibraryError::Overlap))
        ));
        let second = add(&db, &second, "Other").await.unwrap();
        assert!(!second.is_default);
        let listed = list(&db).await.unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].root.id, first.root.id);
        set_default(&db, second.root.id).await.unwrap();
        let listed = list(&db).await.unwrap();
        assert_eq!(listed[0].root.id, second.root.id);
        assert!(listed[0].is_default);
        assert!(!listed[1].is_default);
    }

    #[tokio::test]
    async fn invalid_default_and_label_do_not_change_registration() {
        let dir = worktree_tempdir();
        let db = Db::open_in_memory().unwrap();
        assert!(matches!(
            add(&db, dir.path(), "  ").await,
            Err(LibraryStoreError::InvalidLabel)
        ));
        assert!(matches!(
            set_default(&db, Uuid::now_v7()).await,
            Err(LibraryStoreError::NotFound)
        ));
        assert!(list(&db).await.unwrap().is_empty());
        assert!(!dir.path().join(".vgames-library.json").exists());
    }

    #[tokio::test]
    async fn concurrent_parent_and_child_additions_cannot_both_register() {
        let dir = worktree_tempdir();
        let parent = dir.path().join("parent");
        let child = parent.join("child");
        std::fs::create_dir_all(&child).unwrap();
        let db = Db::open_in_memory().unwrap();
        let (a, b) = tokio::join!(add(&db, &parent, "Parent"), add(&db, &child, "Child"));
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        assert_eq!(list(&db).await.unwrap().len(), 1);
    }
}
