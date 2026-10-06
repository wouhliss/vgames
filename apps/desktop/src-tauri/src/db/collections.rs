//! Local collection storage. All ordering and name checks run on the single
//! SQLite thread, so concurrent requests cannot assign the same position.

use std::collections::HashSet;

use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use super::{Db, DbError, now_unix};
use crate::events::PackageRef;

#[derive(Debug, thiserror::Error)]
pub enum CollectionStoreError {
    #[error("collection name must be between 1 and 100 characters")]
    InvalidName,
    #[error("a collection with this name already exists")]
    NameTaken,
    #[error("collection or server was not found")]
    NotFound,
    #[error("the order contains the same collection twice")]
    DuplicateId,
    #[error("stored collection ID is invalid")]
    InvalidStoredId,
    #[error("too many collections or items")]
    PositionOverflow,
    #[error(transparent)]
    Database(#[from] DbError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub id: Uuid,
    pub name: String,
    pub position: i64,
}

fn checked_name(name: &str) -> Result<String, CollectionStoreError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(CollectionStoreError::InvalidName);
    }
    Ok(name.to_owned())
}

fn name_taken(
    conn: &rusqlite::Connection,
    name: &str,
    except: Option<Uuid>,
) -> Result<bool, CollectionStoreError> {
    let folded = name.to_lowercase();
    let except = except.map(|id| id.to_string());
    let mut query = conn.prepare("SELECT id, name FROM collections")?;
    let rows = query.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (id, existing) = row?;
        if Some(id.as_str()) != except.as_deref() && existing.to_lowercase() == folded {
            return Ok(true);
        }
    }
    Ok(false)
}

fn next_position(conn: &rusqlite::Connection) -> Result<i64, CollectionStoreError> {
    let max: Option<i64> = conn.query_row("SELECT max(position) FROM collections", [], |row| {
        row.get(0)
    })?;
    max.unwrap_or(-1)
        .checked_add(1)
        .ok_or(CollectionStoreError::PositionOverflow)
}

pub async fn list(db: &Db) -> Result<Vec<Collection>, CollectionStoreError> {
    db.call(|conn| {
        Ok((|| -> Result<Vec<Collection>, CollectionStoreError> {
            let mut query =
                conn.prepare("SELECT id, name, position FROM collections ORDER BY position, id")?;
            let rows = query.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            rows.map(|row| {
                let (id, name, position) = row?;
                Ok(Collection {
                    id: Uuid::parse_str(&id).map_err(|_| CollectionStoreError::InvalidStoredId)?,
                    name,
                    position,
                })
            })
            .collect()
        })())
    })
    .await?
}

pub async fn create(db: &Db, name: &str) -> Result<Collection, CollectionStoreError> {
    let name = checked_name(name)?;
    db.call(move |conn| {
        Ok((|| -> Result<Collection, CollectionStoreError> {
            let tx = conn.transaction()?;
            if name_taken(&tx, &name, None)? {
                return Err(CollectionStoreError::NameTaken);
            }
            let collection = Collection {
                id: Uuid::now_v7(),
                name,
                position: next_position(&tx)?,
            };
            tx.execute(
                "INSERT INTO collections (id, name, position, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![
                    collection.id.to_string(),
                    collection.name,
                    collection.position,
                    now_unix()
                ],
            )?;
            tx.commit()?;
            Ok(collection)
        })())
    })
    .await?
}

pub async fn rename(db: &Db, id: Uuid, name: &str) -> Result<Collection, CollectionStoreError> {
    let name = checked_name(name)?;
    db.call(move |conn| {
        Ok((|| -> Result<Collection, CollectionStoreError> {
            let tx = conn.transaction()?;
            let position: i64 = tx
                .query_row(
                    "SELECT position FROM collections WHERE id = ?1",
                    [id.to_string()],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(CollectionStoreError::NotFound)?;
            if name_taken(&tx, &name, Some(id))? {
                return Err(CollectionStoreError::NameTaken);
            }
            tx.execute(
                "UPDATE collections SET name = ?2 WHERE id = ?1",
                params![id.to_string(), name],
            )?;
            tx.commit()?;
            Ok(Collection { id, name, position })
        })())
    })
    .await?
}

pub async fn delete(db: &Db, id: Uuid) -> Result<(), CollectionStoreError> {
    db.call(move |conn| {
        Ok((|| -> Result<(), CollectionStoreError> {
            if conn.execute("DELETE FROM collections WHERE id = ?1", [id.to_string()])? == 0 {
                return Err(CollectionStoreError::NotFound);
            }
            Ok(())
        })())
    })
    .await?
}

/// Places the supplied IDs first in their supplied order; other collections
/// retain their relative order. Refuses duplicates or unknown IDs atomically.
pub async fn reorder(db: &Db, ids: Vec<Uuid>) -> Result<(), CollectionStoreError> {
    if ids.iter().copied().collect::<HashSet<_>>().len() != ids.len() {
        return Err(CollectionStoreError::DuplicateId);
    }
    db.call(move |conn| {
        Ok((|| -> Result<(), CollectionStoreError> {
            let tx = conn.transaction()?;
            let mut query = tx.prepare("SELECT id FROM collections ORDER BY position, id")?;
            let existing = query
                .query_map([], |row| row.get::<_, String>(0))?
                .map(|row| {
                    let id = row?;
                    Uuid::parse_str(&id).map_err(|_| CollectionStoreError::InvalidStoredId)
                })
                .collect::<Result<Vec<_>, _>>()?;
            drop(query);
            let known: HashSet<_> = existing.iter().copied().collect();
            if ids.iter().any(|id| !known.contains(id)) {
                return Err(CollectionStoreError::NotFound);
            }
            let selected: HashSet<_> = ids.iter().copied().collect();
            for (position, id) in ids
                .into_iter()
                .chain(existing.into_iter().filter(|id| !selected.contains(id)))
                .enumerate()
            {
                let position =
                    i64::try_from(position).map_err(|_| CollectionStoreError::PositionOverflow)?;
                tx.execute(
                    "UPDATE collections SET position = ?2 WHERE id = ?1",
                    params![id.to_string(), position],
                )?;
            }
            tx.commit()?;
            Ok(())
        })())
    })
    .await?
}

pub async fn add_package(
    db: &Db,
    id: Uuid,
    package: PackageRef,
) -> Result<(), CollectionStoreError> {
    db.call(move |conn| {
        Ok((|| -> Result<(), CollectionStoreError> {
            let tx = conn.transaction()?;
            let collection_exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM collections WHERE id = ?1)",
                [id.to_string()], |row| row.get(0),
            )?;
            let server_exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM servers WHERE id = ?1)",
                [package.server_id.to_string()], |row| row.get(0),
            )?;
            if !collection_exists || !server_exists {
                return Err(CollectionStoreError::NotFound);
            }
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM collection_items WHERE collection_id = ?1 AND server_id = ?2 AND package_id = ?3)",
                params![id.to_string(), package.server_id.to_string(), package.package_id.to_string()],
                |row| row.get(0),
            )?;
            if !exists {
                let position: Option<i64> = tx.query_row(
                    "SELECT max(position) FROM collection_items WHERE collection_id = ?1",
                    [id.to_string()], |row| row.get(0),
                )?;
                let position = position.unwrap_or(-1).checked_add(1).ok_or(CollectionStoreError::PositionOverflow)?;
                tx.execute(
                    "INSERT INTO collection_items (collection_id, server_id, package_id, position, added_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![id.to_string(), package.server_id.to_string(), package.package_id.to_string(), position, now_unix()],
                )?;
            }
            tx.commit()?;
            Ok(())
        })())
    })
    .await?
}

pub async fn remove_package(
    db: &Db,
    id: Uuid,
    package: PackageRef,
) -> Result<(), CollectionStoreError> {
    db.call(move |conn| {
        Ok((|| -> Result<(), CollectionStoreError> {
            let collection_exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM collections WHERE id = ?1)",
                [id.to_string()], |row| row.get(0),
            )?;
            if !collection_exists {
                return Err(CollectionStoreError::NotFound);
            }
            conn.execute(
                "DELETE FROM collection_items WHERE collection_id = ?1 AND server_id = ?2 AND package_id = ?3",
                params![id.to_string(), package.server_id.to_string(), package.package_id.to_string()],
            )?;
            Ok(())
        })())
    })
    .await?
}

/// Sets a local favorite idempotently. Favorites may refer to catalog packages
/// that are not installed, but the server must still be registered.
pub async fn set_favorite(
    db: &Db,
    package: PackageRef,
    favorite: bool,
) -> Result<(), CollectionStoreError> {
    db.call(move |conn| {
        Ok((|| -> Result<(), CollectionStoreError> {
            let tx = conn.transaction()?;
            let server = package.server_id.to_string();
            let item = package.package_id.to_string();
            if favorite {
                let server_exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM servers WHERE id = ?1)",
                    [&server],
                    |row| row.get(0),
                )?;
                if !server_exists {
                    return Err(CollectionStoreError::NotFound);
                }
                let already_added: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM favorites WHERE server_id = ?1 AND package_id = ?2)",
                    params![server, item], |row| row.get(0),
                )?;
                if !already_added {
                    let max: Option<i64> = tx.query_row(
                        "SELECT max(position) FROM favorites", [], |row| row.get(0),
                    )?;
                    let position = max.unwrap_or(-1).checked_add(1).ok_or(CollectionStoreError::PositionOverflow)?;
                    tx.execute(
                        "INSERT INTO favorites (server_id, package_id, position, created_at) VALUES (?1, ?2, ?3, ?4)",
                        params![server, item, position, now_unix()],
                    )?;
                }
            } else {
                tx.execute(
                    "DELETE FROM favorites WHERE server_id = ?1 AND package_id = ?2",
                    params![server, item],
                )?;
            }
            tx.commit()?;
            Ok(())
        })())
    })
    .await?
}

pub async fn favorite_ids(db: &Db, server_id: Uuid) -> Result<Vec<Uuid>, CollectionStoreError> {
    db.call(move |conn| {
        Ok((|| -> Result<Vec<Uuid>, CollectionStoreError> {
            let mut query = conn.prepare(
                "SELECT package_id FROM favorites WHERE server_id = ?1 ORDER BY position, package_id",
            )?;
            let rows = query.query_map([server_id.to_string()], |row| row.get::<_, String>(0))?;
            rows.map(|row| {
                let id = row?;
                Uuid::parse_str(&id).map_err(|_| CollectionStoreError::InvalidStoredId)
            })
            .collect()
        })())
    })
    .await?
}

/// The collections each package of `server_id` is in, in collection order.
pub async fn memberships(
    db: &Db,
    server_id: Uuid,
) -> Result<std::collections::HashMap<Uuid, Vec<Uuid>>, CollectionStoreError> {
    db.call(move |conn| {
        Ok(
            (|| -> Result<std::collections::HashMap<Uuid, Vec<Uuid>>, CollectionStoreError> {
                let mut query = conn.prepare(
                    "SELECT i.package_id, i.collection_id FROM collection_items i
                   JOIN collections c ON c.id = i.collection_id
                  WHERE i.server_id = ?1 ORDER BY c.position, c.id",
                )?;
                let rows = query.query_map([server_id.to_string()], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;
                let mut out: std::collections::HashMap<Uuid, Vec<Uuid>> =
                    std::collections::HashMap::new();
                for row in rows {
                    let (package, collection) = row?;
                    let package = Uuid::parse_str(&package)
                        .map_err(|_| CollectionStoreError::InvalidStoredId)?;
                    let collection = Uuid::parse_str(&collection)
                        .map_err(|_| CollectionStoreError::InvalidStoredId)?;
                    out.entry(package).or_default().push(collection);
                }
                Ok(out)
            })(),
        )
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn names_are_validated_and_unique_before_writing() {
        let db = Db::open_in_memory().unwrap();
        let first = create(&db, "  Games  ").await.unwrap();
        assert_eq!(first.name, "Games");
        assert!(matches!(
            create(&db, "games").await,
            Err(CollectionStoreError::NameTaken)
        ));
        assert!(matches!(
            create(&db, "\n").await,
            Err(CollectionStoreError::InvalidName)
        ));
        let second = create(&db, "Tools").await.unwrap();
        assert!(matches!(
            rename(&db, second.id, "GAMES").await,
            Err(CollectionStoreError::NameTaken)
        ));
        assert_eq!(list(&db).await.unwrap()[1].name, "Tools");
        assert_eq!(
            rename(&db, first.id, "  Favorites ").await.unwrap().name,
            "Favorites"
        );
    }

    #[tokio::test]
    async fn partial_reorder_preserves_other_items_and_rejects_bad_ids() {
        let db = Db::open_in_memory().unwrap();
        let a = create(&db, "A").await.unwrap();
        let b = create(&db, "B").await.unwrap();
        let c = create(&db, "C").await.unwrap();
        reorder(&db, vec![c.id]).await.unwrap();
        let ids: Vec<_> = list(&db)
            .await
            .unwrap()
            .into_iter()
            .map(|item| item.id)
            .collect();
        assert_eq!(ids, vec![c.id, a.id, b.id]);
        assert!(matches!(
            reorder(&db, vec![a.id, a.id]).await,
            Err(CollectionStoreError::DuplicateId)
        ));
        assert!(matches!(
            reorder(&db, vec![Uuid::now_v7()]).await,
            Err(CollectionStoreError::NotFound)
        ));
        let unchanged: Vec<_> = list(&db)
            .await
            .unwrap()
            .into_iter()
            .map(|item| item.id)
            .collect();
        assert_eq!(unchanged, ids);
    }

    #[tokio::test]
    async fn items_are_idempotent_and_cascade_on_delete() {
        let db = Db::open_in_memory().unwrap();
        let collection = create(&db, "Played").await.unwrap();
        let package = PackageRef {
            server_id: Uuid::now_v7(),
            package_id: Uuid::now_v7(),
        };
        assert!(matches!(
            add_package(&db, collection.id, package).await,
            Err(CollectionStoreError::NotFound)
        ));
        let server_id = package.server_id.to_string();
        db.call(move |conn| {
            conn.execute(
                "INSERT INTO servers (id, url, name, root_public_key, root_fingerprint, added_at)
                 VALUES (?1, 'https://example.test', 'Test', zeroblob(32), 'VG1', 0)",
                [server_id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        add_package(&db, collection.id, package).await.unwrap();
        add_package(&db, collection.id, package).await.unwrap();
        let id = collection.id.to_string();
        let count: i64 = db
            .call(move |conn| {
                Ok(conn.query_row(
                    "SELECT count(*) FROM collection_items WHERE collection_id = ?1",
                    [id],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(count, 1);
        delete(&db, collection.id).await.unwrap();
        assert!(list(&db).await.unwrap().is_empty());
        let count: i64 = db
            .call(|conn| {
                Ok(
                    conn.query_row("SELECT count(*) FROM collection_items", [], |row| {
                        row.get(0)
                    })?,
                )
            })
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn favorites_are_idempotent_and_ordered() {
        let db = Db::open_in_memory().unwrap();
        let server_id = Uuid::now_v7();
        let first = PackageRef {
            server_id,
            package_id: Uuid::now_v7(),
        };
        let second = PackageRef {
            server_id,
            package_id: Uuid::now_v7(),
        };
        assert!(matches!(
            set_favorite(&db, first, true).await,
            Err(CollectionStoreError::NotFound)
        ));
        let id = server_id.to_string();
        db.call(move |conn| {
            conn.execute(
                "INSERT INTO servers (id, url, name, root_public_key, root_fingerprint, added_at)
                 VALUES (?1, 'https://example.test', 'Test', zeroblob(32), 'VG1', 0)",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        set_favorite(&db, first, true).await.unwrap();
        set_favorite(&db, second, true).await.unwrap();
        set_favorite(&db, first, true).await.unwrap();
        assert_eq!(
            favorite_ids(&db, server_id).await.unwrap(),
            vec![first.package_id, second.package_id]
        );
        set_favorite(&db, first, false).await.unwrap();
        set_favorite(&db, first, false).await.unwrap();
        assert_eq!(
            favorite_ids(&db, server_id).await.unwrap(),
            vec![second.package_id]
        );
    }
}
