//! Embedded, append-only schema migrations, versioned by `PRAGMA user_version`
//! (= number of applied migrations). Each migration runs in one transaction
//! together with the version bump, so a crash never leaves a half-migrated
//! schema. `schema_migrations` records each applied migration's name and
//! BLAKE3, and startup refuses a database whose history does not match this
//! build (an edited or reordered migration).
//!
//! **Registration hook (Agent 4):** social tables are added by appending
//! `Migration { name: "NNNN_social_…", sql: include_str!("../social/migrations/NNNN_….sql") }`
//! to [`MIGRATIONS`], in order, after whatever is already there. Never insert
//! in the middle and never edit a released entry.

use rusqlite::{Connection, OptionalExtension};

use super::DbError;

/// One schema step.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// Unique, stable name (`NNNN_topic`).
    pub name: &'static str,
    pub sql: &'static str,
}

/// Every migration, in application order.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        name: "0001_init",
        sql: include_str!("migrations/0001_init.sql"),
    },
    // Append new migrations (core or social) below this line.
    Migration {
        name: "0002_social",
        sql: include_str!("../social/migrations/0002_social.sql"),
    },
    Migration {
        name: "0003_servers_trust",
        sql: include_str!("migrations/0003_servers_trust.sql"),
    },
];

fn checksum(sql: &str) -> String {
    blake3::hash(sql.as_bytes()).to_hex().to_string()
}

/// Applies pending migrations. Idempotent.
pub fn run(conn: &mut Connection) -> Result<(), DbError> {
    run_list(conn, MIGRATIONS)
}

pub(crate) fn run_list(conn: &mut Connection, list: &[Migration]) -> Result<(), DbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version     INTEGER PRIMARY KEY,
             name        TEXT NOT NULL,
             checksum    TEXT NOT NULL,
             applied_at  INTEGER NOT NULL
         )",
    )?;
    let current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let known = u32::try_from(list.len()).unwrap_or(u32::MAX);
    if current > known {
        return Err(DbError::TooNew {
            found: current,
            known,
        });
    }

    for (index, migration) in list.iter().enumerate() {
        let version = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let expected = format!("{} {}", migration.name, checksum(migration.sql));
        if version <= current {
            let found: Option<String> = conn
                .query_row(
                    "SELECT name || ' ' || checksum FROM schema_migrations WHERE version = ?1",
                    [version],
                    |r| r.get(0),
                )
                .optional()?;
            let found = found.unwrap_or_else(|| "nothing".to_owned());
            if found != expected {
                return Err(DbError::MigrationMismatch {
                    index,
                    expected,
                    found,
                });
            }
            continue;
        }

        let tx = conn.transaction()?;
        tx.execute_batch(migration.sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name, checksum, applied_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![version, migration.name, checksum(migration.sql), super::now_unix()],
        )?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
        tracing::info!(version, name = migration.name, "applied database migration");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(conn: &Connection) -> u32 {
        conn.query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
    }

    fn tables(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn names_are_unique_and_ordered() {
        let names: Vec<_> = MIGRATIONS.iter().map(|m| m.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names, sorted, "migration names must be unique and in order");
    }

    #[test]
    fn fresh_database_gets_every_table() {
        let mut conn = Connection::open_in_memory().unwrap();
        run(&mut conn).unwrap();
        assert_eq!(version(&conn) as usize, MIGRATIONS.len());
        let tables = tables(&conn);
        for expected in [
            "servers",
            "accounts",
            "libraries",
            "installs",
            "favorites",
            "collections",
            "collection_items",
            "settings",
            "download_jobs",
            "save_sync_state",
            "controller_profiles",
            "shortcuts",
            "schema_migrations",
        ] {
            assert!(tables.iter().any(|t| t == expected), "missing {expected}");
        }
    }

    #[test]
    fn running_twice_is_a_no_op() {
        let mut conn = Connection::open_in_memory().unwrap();
        run(&mut conn).unwrap();
        run(&mut conn).unwrap();
        assert_eq!(version(&conn) as usize, MIGRATIONS.len());
    }

    const V1: Migration = Migration {
        name: "0001_a",
        sql: "CREATE TABLE a (x INTEGER); INSERT INTO a VALUES (1);",
    };
    const V2: Migration = Migration {
        name: "0002_b",
        sql: "ALTER TABLE a ADD COLUMN y INTEGER NOT NULL DEFAULT 7;",
    };

    #[test]
    fn upgrade_path_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("u.sqlite3");
        {
            let mut conn = Connection::open(&path).unwrap();
            run_list(&mut conn, &[V1]).unwrap();
            assert_eq!(version(&conn), 1);
        }
        let mut conn = Connection::open(&path).unwrap();
        run_list(&mut conn, &[V1, V2]).unwrap();
        assert_eq!(version(&conn), 2);
        let (x, y): (i64, i64) = conn
            .query_row("SELECT x, y FROM a", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((x, y), (1, 7));
    }

    #[test]
    fn failed_migration_rolls_back_completely() {
        let broken = Migration {
            name: "0002_broken",
            sql: "CREATE TABLE b (x INTEGER); THIS IS NOT SQL;",
        };
        let mut conn = Connection::open_in_memory().unwrap();
        assert!(run_list(&mut conn, &[V1, broken]).is_err());
        assert_eq!(version(&conn), 1);
        assert!(!tables(&conn).iter().any(|t| t == "b"));
    }

    #[test]
    fn edited_migration_is_refused() {
        let mut conn = Connection::open_in_memory().unwrap();
        run_list(&mut conn, &[V1]).unwrap();
        let edited = Migration {
            name: "0001_a",
            sql: "CREATE TABLE a (x TEXT);",
        };
        assert!(matches!(
            run_list(&mut conn, &[edited]),
            Err(DbError::MigrationMismatch { index: 0, .. })
        ));
    }

    #[test]
    fn newer_database_is_refused() {
        let mut conn = Connection::open_in_memory().unwrap();
        run_list(&mut conn, &[V1, V2]).unwrap();
        assert!(matches!(
            run_list(&mut conn, &[V1]),
            Err(DbError::TooNew { found: 2, known: 1 })
        ));
    }
}
