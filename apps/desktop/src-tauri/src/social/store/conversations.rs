//! The conversation list cache (A4-T08). The server is the source of truth for membership;
//! this cache lets the launcher address members' devices, order the list by activity and
//! count unread messages while offline.

use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use super::{Keys, MESSAGE_COLUMNS, StoreError, id, map_message, message_from, text};
use crate::social::model::{Conversation, ConversationKind, UserSummary, unix_rfc3339};

/// A conversation as the server lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedConversation {
    pub id: Uuid,
    pub kind: ConversationKind,
    pub members: Vec<UserSummary>,
    pub created_at: i64,
    pub last_activity_at: i64,
}

fn kind_str(kind: ConversationKind) -> &'static str {
    match kind {
        ConversationKind::Direct => "direct",
        ConversationKind::Party => "party",
    }
}

fn upsert(conn: &Connection, server: Uuid, c: &CachedConversation) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO social_conversations (server_id, id, kind, members, created_at, last_activity_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (server_id, id) DO UPDATE SET kind = excluded.kind, members = excluded.members,
           last_activity_at = max(last_activity_at, excluded.last_activity_at)",
        params![
            text(server),
            text(c.id),
            kind_str(c.kind),
            serde_json::to_string(&c.members)?,
            c.created_at,
            c.last_activity_at
        ],
    )?;
    Ok(())
}

/// Adds or updates one conversation (after opening or creating it).
pub fn upsert_conversation(
    conn: &Connection,
    server: Uuid,
    c: &CachedConversation,
) -> Result<(), StoreError> {
    upsert(conn, server, c)
}

/// Replaces the cached list with the server's full list. Conversations the user left
/// disappear from the list; their messages stay in the history.
pub fn replace_conversations(
    conn: &mut Connection,
    server: Uuid,
    list: &[CachedConversation],
) -> Result<(), StoreError> {
    let tx = conn.transaction()?;
    let keep: Vec<String> = list.iter().map(|c| text(c.id)).collect();
    tx.execute(
        "DELETE FROM social_conversations WHERE server_id = ?1
           AND id NOT IN (SELECT value FROM json_each(?2))",
        params![text(server), serde_json::to_string(&keep)?],
    )?;
    for c in list {
        upsert(&tx, server, c)?;
    }
    tx.commit()?;
    Ok(())
}

/// Kind and members of a cached conversation.
pub fn conversation_members(
    conn: &Connection,
    server: Uuid,
    conversation: Uuid,
) -> Result<Option<(ConversationKind, Vec<UserSummary>)>, StoreError> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT kind, members FROM social_conversations WHERE server_id = ?1 AND id = ?2",
            params![text(server), text(conversation)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((kind, members)) = row else {
        return Ok(None);
    };
    let kind = match kind.as_str() {
        "direct" => ConversationKind::Direct,
        "party" => ConversationKind::Party,
        _ => return Err(StoreError::Corrupt),
    };
    Ok(Some((kind, serde_json::from_str(&members)?)))
}

/// Cached conversations `user` is a member of.
pub fn conversations_with(
    conn: &Connection,
    server: Uuid,
    user: Uuid,
) -> Result<Vec<Uuid>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT c.id FROM social_conversations c
         WHERE c.server_id = ?1
           AND EXISTS (SELECT 1 FROM json_each(c.members) m WHERE json_extract(m.value, '$.id') = ?2)
         ORDER BY c.last_activity_at DESC, c.id",
    )?;
    let rows = stmt
        .query_map(params![text(server), text(user)], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    rows.iter().map(|s| id(s)).collect()
}

/// `true` when a message is stored for a conversation the cache does not know.
pub fn is_cached(conn: &Connection, server: Uuid, conversation: Uuid) -> Result<bool, StoreError> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM social_conversations WHERE server_id = ?1 AND id = ?2)",
        params![text(server), text(conversation)],
        |r| r.get(0),
    )?)
}

fn build(
    conn: &Connection,
    keys: &Keys,
    server: Uuid,
    row: (String, String, String, i64, i64),
) -> Result<Conversation, StoreError> {
    let (cid, kind, members, created_at, last_read_seq) = row;
    let conversation = id(&cid)?;
    let last = conn
        .query_row(
            &format!(
                "SELECT {MESSAGE_COLUMNS} FROM social_messages WHERE server_id = ?1 AND conversation_id = ?2
                 ORDER BY seq DESC LIMIT 1"
            ),
            params![text(server), cid],
            map_message,
        )
        .optional()?
        .map(|t| message_from(keys, server, t))
        .transpose()?;
    let unread: i64 = conn.query_row(
        "SELECT count(*) FROM social_messages WHERE server_id = ?1 AND conversation_id = ?2
           AND direction = 'in' AND seq > ?3",
        params![text(server), cid, last_read_seq],
        |r| r.get(0),
    )?;
    Ok(Conversation {
        id: conversation,
        kind: match kind.as_str() {
            "direct" => ConversationKind::Direct,
            "party" => ConversationKind::Party,
            _ => return Err(StoreError::Corrupt),
        },
        members: serde_json::from_str(&members)?,
        last_message: last,
        unread: u32::try_from(unread).unwrap_or(u32::MAX),
        created_at: unix_rfc3339(created_at),
    })
}

const CONVERSATION_COLUMNS: &str = "id, kind, members, created_at, last_read_seq";

type ConversationTuple = (String, String, String, i64, i64);

fn map_conversation(r: &rusqlite::Row<'_>) -> rusqlite::Result<ConversationTuple> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
}

/// The cached conversations, most recent activity first, with their last message and
/// unread count.
pub fn list_conversations(
    conn: &Connection,
    keys: &Keys,
    server: Uuid,
) -> Result<Vec<Conversation>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CONVERSATION_COLUMNS} FROM social_conversations WHERE server_id = ?1
         ORDER BY last_activity_at DESC, created_at DESC, id"
    ))?;
    let rows = stmt
        .query_map([text(server)], map_conversation)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|row| build(conn, keys, server, row))
        .collect()
}

/// One cached conversation.
pub fn get_conversation(
    conn: &Connection,
    keys: &Keys,
    server: Uuid,
    conversation: Uuid,
) -> Result<Option<Conversation>, StoreError> {
    conn.query_row(
        &format!(
            "SELECT {CONVERSATION_COLUMNS} FROM social_conversations WHERE server_id = ?1 AND id = ?2"
        ),
        params![text(server), text(conversation)],
        map_conversation,
    )
    .optional()?
    .map(|row| build(conn, keys, server, row))
    .transpose()
}

/// Marks every message of the conversation as read. Returns `false` when it is not cached.
pub fn mark_read(conn: &Connection, server: Uuid, conversation: Uuid) -> Result<bool, StoreError> {
    let n = conn.execute(
        "UPDATE social_conversations SET last_read_seq = coalesce(
           (SELECT max(seq) FROM social_messages WHERE server_id = ?1 AND conversation_id = ?2), last_read_seq)
         WHERE server_id = ?1 AND id = ?2",
        params![text(server), text(conversation)],
    )?;
    Ok(n > 0)
}
