//! `servers` and `accounts` rows (run on the database thread).

use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use super::{Account, Role};
use crate::db::{DbError, now_unix};

/// A stored server, with its signed-in account if any.
#[derive(Debug, Clone)]
pub struct ServerRow {
    pub id: Uuid,
    pub url: String,
    pub name: String,
    pub root_public_key: [u8; 32],
    pub root_fingerprint: String,
    pub trust_version: Option<u64>,
    pub trust_bundle: Option<Vec<u8>>,
    pub trust_signature: Option<Vec<u8>>,
    pub last_connected_at: Option<i64>,
    pub active: bool,
    pub blocked_fingerprint: Option<String>,
    pub account: Option<Account>,
}

const SELECT: &str =
    "SELECT s.id, s.url, s.name, s.root_public_key, s.root_fingerprint, s.trust_version,
        s.trust_bundle, s.trust_signature, s.last_connected_at, s.is_active, s.blocked_fingerprint,
        a.user_id, a.username, a.display_name, a.role
    FROM servers s LEFT JOIN accounts a ON a.server_id = s.id";

fn parse_uuid(index: usize, text: &str) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<ServerRow> {
    let id: String = row.get(0)?;
    let key: Vec<u8> = row.get(3)?;
    let root_public_key: [u8; 32] = key.as_slice().try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Blob,
            "root key is not 32 bytes".into(),
        )
    })?;
    let trust_version: Option<i64> = row.get(5)?;
    let user_id: Option<String> = row.get(11)?;
    let account = match user_id {
        Some(user_id) => {
            let role: String = row.get(14)?;
            Some(Account {
                user_id: parse_uuid(11, &user_id)?,
                username: row.get(12)?,
                display_name: row.get(13)?,
                role: match role.as_str() {
                    "owner" => Role::Owner,
                    "admin" => Role::Admin,
                    _ => Role::User,
                },
            })
        }
        None => None,
    };
    Ok(ServerRow {
        id: parse_uuid(0, &id)?,
        url: row.get(1)?,
        name: row.get(2)?,
        root_public_key,
        root_fingerprint: row.get(4)?,
        trust_version: trust_version.and_then(|v| u64::try_from(v).ok()),
        trust_bundle: row.get(6)?,
        trust_signature: row.get(7)?,
        last_connected_at: row.get(8)?,
        active: row.get(9)?,
        blocked_fingerprint: row.get(10)?,
        account,
    })
}

pub fn list(conn: &Connection) -> Result<Vec<ServerRow>, DbError> {
    let mut stmt = conn.prepare(&format!("{SELECT} ORDER BY s.added_at, s.name"))?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn get(conn: &Connection, id: Uuid) -> Result<Option<ServerRow>, DbError> {
    Ok(conn
        .query_row(
            &format!("{SELECT} WHERE s.id = ?1"),
            [id.to_string()],
            from_row,
        )
        .optional()?)
}

/// The server with this id or this URL, if one is stored.
pub fn find(conn: &Connection, id: Uuid, url: &str) -> Result<Option<ServerRow>, DbError> {
    Ok(conn
        .query_row(
            &format!("{SELECT} WHERE s.id = ?1 OR s.url = ?2 ORDER BY s.id = ?1 DESC LIMIT 1"),
            params![id.to_string(), url],
            from_row,
        )
        .optional()?)
}

pub fn active(conn: &Connection) -> Result<Option<ServerRow>, DbError> {
    Ok(conn
        .query_row(&format!("{SELECT} WHERE s.is_active = 1"), [], from_row)
        .optional()?)
}

/// A newly pinned server (TOFU or `fp=` link); becomes the active one.
pub struct NewServer<'a> {
    pub id: Uuid,
    pub url: &'a str,
    pub name: &'a str,
    pub root_public_key: &'a [u8; 32],
    pub root_fingerprint: &'a str,
}

pub fn insert_active(conn: &mut Connection, server: &NewServer<'_>) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    tx.execute("UPDATE servers SET is_active = 0 WHERE is_active = 1", [])?;
    tx.execute(
        "INSERT INTO servers (id, url, name, root_public_key, root_fingerprint, added_at, is_active)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
        params![
            server.id.to_string(),
            server.url,
            server.name,
            server.root_public_key.as_slice(),
            server.root_fingerprint,
            now_unix()
        ],
    )?;
    tx.commit()?;
    Ok(())
}

/// Makes `id` the only active server. Returns false when it does not exist.
pub fn set_active(conn: &mut Connection, id: Uuid) -> Result<bool, DbError> {
    let tx = conn.transaction()?;
    tx.execute("UPDATE servers SET is_active = 0 WHERE is_active = 1", [])?;
    let changed = tx.execute(
        "UPDATE servers SET is_active = 1 WHERE id = ?1",
        [id.to_string()],
    )?;
    if changed == 0 {
        return Ok(false);
    }
    tx.commit()?;
    Ok(true)
}

/// Why a server could not be removed.
pub enum RemoveOutcome {
    Removed { was_active: bool },
    NotFound,
    HasInstalls,
}

pub fn remove(conn: &mut Connection, id: Uuid) -> Result<RemoveOutcome, DbError> {
    let tx = conn.transaction()?;
    let installs: i64 = tx.query_row(
        "SELECT count(*) FROM installs WHERE server_id = ?1",
        [id.to_string()],
        |r| r.get(0),
    )?;
    if installs > 0 {
        return Ok(RemoveOutcome::HasInstalls);
    }
    let was_active: Option<bool> = tx
        .query_row(
            "SELECT is_active FROM servers WHERE id = ?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(was_active) = was_active else {
        return Ok(RemoveOutcome::NotFound);
    };
    tx.execute("DELETE FROM servers WHERE id = ?1", [id.to_string()])?;
    tx.commit()?;
    Ok(RemoveOutcome::Removed { was_active })
}

/// A verified trust bundle and the (possibly rotated) root it chains to.
pub struct TrustUpdate<'a> {
    pub version: u64,
    pub bundle: &'a [u8],
    pub signature: &'a [u8; 64],
    pub root_public_key: &'a [u8; 32],
    pub root_fingerprint: &'a str,
}

/// Stores the bundle unless a higher version is already stored (the
/// comparison is repeated here so two concurrent refreshes cannot go back).
pub fn save_trust(conn: &Connection, id: Uuid, update: &TrustUpdate<'_>) -> Result<bool, DbError> {
    let version = i64::try_from(update.version).unwrap_or(i64::MAX);
    let changed = conn.execute(
        "UPDATE servers SET trust_version = ?2, trust_bundle = ?3, trust_signature = ?4,
                root_public_key = ?5, root_fingerprint = ?6
         WHERE id = ?1 AND (trust_version IS NULL OR trust_version <= ?2)",
        params![
            id.to_string(),
            version,
            update.bundle,
            update.signature.as_slice(),
            update.root_public_key.as_slice(),
            update.root_fingerprint
        ],
    )?;
    Ok(changed == 1)
}

pub fn set_blocked(conn: &Connection, id: Uuid, presented: Option<&str>) -> Result<(), DbError> {
    conn.execute(
        "UPDATE servers SET blocked_fingerprint = ?2 WHERE id = ?1",
        params![id.to_string(), presented],
    )?;
    Ok(())
}

/// A successful identity check: refresh the display name and the timestamp.
pub fn touch_connected(conn: &Connection, id: Uuid, name: &str) -> Result<(), DbError> {
    conn.execute(
        "UPDATE servers SET name = ?2, last_connected_at = ?3 WHERE id = ?1",
        params![id.to_string(), name, now_unix()],
    )?;
    Ok(())
}

pub fn upsert_account(
    conn: &Connection,
    server_id: Uuid,
    account: &Account,
) -> Result<(), DbError> {
    let role = match account.role {
        Role::User => "user",
        Role::Admin => "admin",
        Role::Owner => "owner",
    };
    conn.execute(
        "INSERT INTO accounts (server_id, user_id, username, display_name, role, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (server_id) DO UPDATE SET user_id = excluded.user_id,
             username = excluded.username, display_name = excluded.display_name,
             role = excluded.role, updated_at = excluded.updated_at",
        params![
            server_id.to_string(),
            account.user_id.to_string(),
            account.username,
            account.display_name,
            role,
            now_unix()
        ],
    )?;
    Ok(())
}

pub fn delete_account(conn: &Connection, server_id: Uuid) -> Result<(), DbError> {
    conn.execute(
        "DELETE FROM accounts WHERE server_id = ?1",
        [server_id.to_string()],
    )?;
    Ok(())
}
