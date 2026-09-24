//! Typed key/value settings. Each setting is a type implementing [`Setting`]:
//! its key, its value type (stored as JSON) and its default. Unknown or
//! unreadable stored values fall back to the default and are logged, so a
//! downgrade or a corrupt row never blocks startup.

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{DbError, now_unix};

pub trait Setting {
    const KEY: &'static str;
    type Value: Serialize + DeserializeOwned;
    fn default_value() -> Self::Value;
}

/// Reads a setting, falling back to its default.
pub fn get<S: Setting>(conn: &Connection) -> Result<S::Value, DbError> {
    let raw: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [S::KEY], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(match raw {
        None => S::default_value(),
        Some(raw) => serde_json::from_str(&raw).unwrap_or_else(|error| {
            tracing::warn!(key = S::KEY, %error, "ignoring an unreadable setting");
            S::default_value()
        }),
    })
}

pub fn set<S: Setting>(conn: &Connection, value: &S::Value) -> Result<(), DbError> {
    let json = serde_json::to_string(value)?;
    conn.execute(
        "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        rusqlite::params![S::KEY, json, now_unix()],
    )?;
    Ok(())
}

/// Number of installs that download at the same time (A2-T08; default 1).
pub struct ConcurrentInstalls;
impl Setting for ConcurrentInstalls {
    const KEY: &'static str = "installs.concurrent";
    type Value = u32;
    fn default_value() -> u32 {
        1
    }
}

/// Download bandwidth cap in bytes per second; `None` = unlimited (02 §7.12).
pub struct DownloadLimit;
impl Setting for DownloadLimit {
    const KEY: &'static str = "downloads.limit_bytes_per_second";
    type Value = Option<u64>;
    fn default_value() -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        super::super::migrations::run(&mut conn).unwrap();
        conn
    }

    #[test]
    fn defaults_then_round_trip() {
        let conn = conn();
        assert_eq!(get::<ConcurrentInstalls>(&conn).unwrap(), 1);
        assert_eq!(get::<DownloadLimit>(&conn).unwrap(), None);
        set::<DownloadLimit>(&conn, &Some(5_000_000)).unwrap();
        set::<DownloadLimit>(&conn, &Some(6_000_000)).unwrap();
        assert_eq!(get::<DownloadLimit>(&conn).unwrap(), Some(6_000_000));
    }

    #[test]
    fn unreadable_value_falls_back_to_default() {
        let conn = conn();
        conn.execute(
            "INSERT INTO settings (key, value, updated_at) VALUES ('installs.concurrent', '\"many\"', 0)",
            [],
        )
        .unwrap();
        assert_eq!(get::<ConcurrentInstalls>(&conn).unwrap(), 1);
    }
}
