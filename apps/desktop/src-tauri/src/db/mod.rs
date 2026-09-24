//! Local SQLite database (A2-T02).
//!
//! One connection lives on a dedicated thread. Callers send closures through
//! [`Db::call`] and await the result, so SQLite never blocks the async runtime
//! or the UI thread, and there is no lock contention between connections. The
//! thread sleeps in a blocking `recv()` while idle and exits when the last
//! [`Db`] handle is dropped.
//!
//! Pragmas: WAL (readers never block the writer, and a crash mid-write leaves
//! the last committed state), `foreign_keys=ON`, `busy_timeout`,
//! `synchronous=NORMAL` (consistent after a crash; the last commits may be
//! lost on power failure, and every such row can be rebuilt from install
//! records on disk).

pub mod migrations;
pub mod settings;

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use rusqlite::Connection;
use tokio::sync::oneshot;

pub use rusqlite;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error")]
    Sqlite(#[from] rusqlite::Error),
    #[error("stored JSON is invalid")]
    Json(#[from] serde_json::Error),
    #[error("the database thread has stopped")]
    Closed,
    #[error("cannot start the database thread")]
    Thread(#[source] std::io::Error),
    #[error(
        "the database was created by a newer launcher (schema {found}, this build knows {known})"
    )]
    TooNew { found: u32, known: u32 },
    #[error("migration {index} was changed after release (expected {expected}, found {found})")]
    MigrationMismatch {
        index: usize,
        expected: String,
        found: String,
    },
}

/// Cloneable handle to the database thread.
#[derive(Debug, Clone)]
pub struct Db {
    jobs: mpsc::Sender<Job>,
}

impl Db {
    /// Opens (creating if needed) and migrates the database file.
    pub fn open(path: &Path) -> Result<Self, DbError> {
        let path = path.to_owned();
        Self::start(move || Connection::open(path))
    }

    /// A private in-memory database, for tests.
    pub fn open_in_memory() -> Result<Self, DbError> {
        Self::start(Connection::open_in_memory)
    }

    fn start<F>(open: F) -> Result<Self, DbError>
    where
        F: FnOnce() -> rusqlite::Result<Connection> + Send + 'static,
    {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), DbError>>(1);
        std::thread::Builder::new()
            .name("vgames-db".into())
            .spawn(move || {
                let mut conn = match open().map_err(DbError::from).and_then(|mut c| {
                    configure(&c)?;
                    migrations::run(&mut c)?;
                    Ok(c)
                }) {
                    Ok(conn) => {
                        let _ = ready_tx.send(Ok(()));
                        conn
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                // Blocks while idle; ends when every `Db` handle is dropped.
                while let Ok(job) = inbox.recv() {
                    job(&mut conn);
                }
            })
            .map_err(DbError::Thread)?;
        ready_rx.recv().map_err(|_| DbError::Closed)??;
        Ok(Self { jobs })
    }

    /// Runs `f` on the database thread and returns its result.
    pub async fn call<T, F>(&self, f: F) -> Result<T, DbError>
    where
        F: FnOnce(&mut Connection) -> Result<T, DbError> + Send + 'static,
        T: Send + 'static,
    {
        let (reply, result) = oneshot::channel();
        self.jobs
            .send(Box::new(move |conn| {
                let _ = reply.send(f(conn));
            }))
            .map_err(|_| DbError::Closed)?;
        result.await.map_err(|_| DbError::Closed)?
    }
}

fn configure(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    // `journal_mode` returns the resulting mode; in-memory databases stay `memory`.
    let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

/// Current time as Unix seconds, the time format of every `*_at` column.
pub fn now_unix() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[tokio::test]
    async fn call_runs_on_the_db_thread() {
        let db = Db::open_in_memory().unwrap();
        let name = db
            .call(|_| Ok(std::thread::current().name().map(str::to_owned)))
            .await
            .unwrap();
        assert_eq!(name.as_deref(), Some("vgames-db"));
    }

    #[tokio::test]
    async fn pragmas_are_applied() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite3")).unwrap();
        let (mode, fk): (String, i64) = db
            .call(|c| {
                let mode = c.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
                let fk = c.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
                Ok((mode, fk))
            })
            .await
            .unwrap();
        assert_eq!(mode, "wal");
        assert_eq!(fk, 1);
    }

    #[tokio::test]
    async fn foreign_keys_are_enforced() {
        let db = Db::open_in_memory().unwrap();
        let result = db
            .call(|c| {
                c.execute(
                    "INSERT INTO accounts (server_id, user_id, username, role, updated_at)
                     VALUES ('missing', 'u', 'n', 'user', 0)",
                    [],
                )?;
                Ok(())
            })
            .await;
        assert!(matches!(result, Err(DbError::Sqlite(_))));
    }

    #[tokio::test]
    async fn only_one_active_server() {
        let db = Db::open_in_memory().unwrap();
        let second_active = db
            .call(|c| {
                for (id, active) in [("a", 1), ("b", 1)] {
                    c.execute(
                        "INSERT INTO servers (id, url, name, root_public_key, root_fingerprint, added_at, is_active)
                         VALUES (?1, ?1, ?1, zeroblob(32), 'VG1', 0, ?2)",
                        rusqlite::params![id, active],
                    )?;
                }
                Ok(())
            })
            .await;
        assert!(second_active.is_err());
    }

    #[tokio::test]
    async fn handles_are_shared_and_errors_do_not_kill_the_thread() {
        let db = Db::open_in_memory().unwrap();
        let other = db.clone();
        assert!(db.call(|c| Ok(c.execute("NOT SQL", [])?)).await.is_err());
        let n = other.call(|c| Ok(count(c, "servers"))).await.unwrap();
        assert_eq!(n, 0);
    }

    const CRASH_ENV: &str = "VGAMES_DB_CRASH_CHILD";
    const BATCH: i64 = 500;

    /// Child side of `crash_mid_write_leaves_a_consistent_db`: writes batches
    /// in transactions until killed. Does nothing in a normal test run.
    #[test]
    fn crash_writer_child() {
        let Some(path) = std::env::var_os(CRASH_ENV) else {
            return;
        };
        let mut conn = Connection::open(path).unwrap();
        configure(&conn).unwrap();
        migrations::run(&mut conn).unwrap();
        for batch in 0..u64::MAX {
            let tx = conn.transaction().unwrap();
            for i in 0..BATCH {
                tx.execute(
                    "INSERT INTO settings (key, value, updated_at) VALUES (?1, '{\"pad\":\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"}', 0)",
                    [format!("k{batch}-{i}")],
                )
                .unwrap();
            }
            tx.commit().unwrap();
            if batch == 0 {
                // Tell the parent that writing has started.
                std::fs::write(
                    format!("{}.started", std::env::var(CRASH_ENV).unwrap()),
                    b"",
                )
                .unwrap();
            }
        }
    }

    #[test]
    fn crash_mid_write_leaves_a_consistent_db() {
        if std::env::var_os(CRASH_ENV).is_some() {
            return;
        }
        let exe = std::env::current_exe().unwrap();
        for round in 0..5u64 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("crash.sqlite3");
            let mut child = std::process::Command::new(&exe)
                .args([
                    "db::tests::crash_writer_child",
                    "--exact",
                    "--test-threads=1",
                ])
                .env(CRASH_ENV, &path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let started = path.with_file_name("crash.sqlite3.started");
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            while !started.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "child never started writing"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            // Kill at a different point in each round, mid-transaction most of the time.
            std::thread::sleep(Duration::from_millis(20 + round * 37));
            child.kill().unwrap();
            child.wait().unwrap();

            let conn = Connection::open(&path).unwrap();
            let integrity: String = conn
                .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .unwrap();
            assert_eq!(integrity, "ok");
            let rows = count(&conn, "settings");
            assert!(rows >= BATCH, "at least the first batch committed");
            assert_eq!(rows % BATCH, 0, "a partial transaction survived the crash");
        }
    }
}
