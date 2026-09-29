//! Local invite state (A4-T09): the sender's join secret, sealed at rest, and which invites
//! this install accepted. The server never sees the secret; the WebView never gets it.

use rusqlite::{Connection, OptionalExtension, params};
use time::OffsetDateTime;
use uuid::Uuid;
use vgames_proto::social::is_valid_join_secret;

use super::{Keys, StoreError, create_outgoing, text};
use crate::social::model::{Message, MessageBody};
use crate::social::payload::Payload;

/// `(nonce, ciphertext)` of a stored join secret; both absent without a secret.
type SealedSecret = (Option<Vec<u8>>, Option<Vec<u8>>);

fn secret_aad(server: Uuid, invite: Uuid) -> Vec<u8> {
    let mut aad = b"vgames invite join secret v1\0".to_vec();
    aad.extend_from_slice(server.as_bytes());
    aad.extend_from_slice(invite.as_bytes());
    aad
}

/// This install created `invite`; `secret` (already validated) is kept for `invite.join`.
/// Creating the same invite again (the server returns the active one) replaces the secret.
pub fn invite_sent(
    conn: &Connection,
    keys: &Keys,
    server: Uuid,
    invite: Uuid,
    package: Uuid,
    secret: Option<&str>,
    now: i64,
) -> Result<(), StoreError> {
    let sealed = match secret {
        Some(s) => {
            if !is_valid_join_secret(s) {
                return Err(StoreError::Corrupt);
            }
            let (nonce, sealed) = keys.body.seal(&secret_aad(server, invite), s.as_bytes())?;
            Some((nonce.to_vec(), sealed))
        }
        None => None,
    };
    let (nonce, sealed) = sealed.unzip();
    conn.execute(
        "INSERT INTO social_invites_local (server_id, invite_id, package_id, role, secret_nonce, secret, created_at)
         VALUES (?1, ?2, ?3, 'sent', ?4, ?5, ?6)
         ON CONFLICT (server_id, invite_id) DO UPDATE SET secret_nonce = excluded.secret_nonce,
           secret = excluded.secret, join_sent = 0
         WHERE role = 'sent'",
        params![text(server), text(invite), text(package), nonce, sealed, now],
    )?;
    Ok(())
}

/// This install accepted `invite` (only it launches the game on `invite.join`).
pub fn invite_accepted(
    conn: &Connection,
    server: Uuid,
    invite: Uuid,
    package: Uuid,
    now: i64,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT OR IGNORE INTO social_invites_local (server_id, invite_id, package_id, role, created_at)
         VALUES (?1, ?2, ?3, 'accepted', ?4)",
        params![text(server), text(invite), text(package), now],
    )?;
    Ok(())
}

/// The local role of an invite, if this install sent or accepted it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalInvite {
    Sent { has_secret: bool, join_sent: bool },
    Accepted,
}

pub fn local_invite(
    conn: &Connection,
    server: Uuid,
    invite: Uuid,
) -> Result<Option<LocalInvite>, StoreError> {
    let row: Option<(String, bool, bool)> = conn
        .query_row(
            "SELECT role, secret IS NOT NULL, join_sent FROM social_invites_local
             WHERE server_id = ?1 AND invite_id = ?2",
            params![text(server), text(invite)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(
        row.map(|(role, has_secret, join_sent)| match role.as_str() {
            "sent" => LocalInvite::Sent {
                has_secret,
                join_sent,
            },
            _ => LocalInvite::Accepted,
        }),
    )
}

/// Queues `invite.join` (with the stored secret, if any) to `conversation` once. Returns the
/// stored message, or `None` when this install did not send the invite or already queued it.
/// The secret goes into the outbox payload (sealed at rest) and nowhere else.
pub fn queue_invite_join(
    conn: &mut Connection,
    keys: &Keys,
    server: Uuid,
    invite: Uuid,
    conversation: Uuid,
    now: i64,
) -> Result<Option<Message>, StoreError> {
    let row: Option<SealedSecret> = conn
        .query_row(
            "SELECT secret_nonce, secret FROM social_invites_local
             WHERE server_id = ?1 AND invite_id = ?2 AND role = 'sent' AND join_sent = 0",
            params![text(server), text(invite)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((nonce, sealed)) = row else {
        return Ok(None);
    };
    let secret = match (nonce, sealed) {
        (Some(nonce), Some(sealed)) => {
            let bytes = keys
                .body
                .open(&secret_aad(server, invite), &nonce, &sealed)?;
            Some(String::from_utf8(bytes).map_err(|_| StoreError::Corrupt)?)
        }
        _ => None,
    };
    let sent_at = OffsetDateTime::from_unix_timestamp(now).map_err(|_| StoreError::Corrupt)?;
    let payload = Payload::invite_join(conversation, Uuid::now_v7(), sent_at, invite, secret)?;
    let message = create_outgoing(
        conn,
        keys,
        server,
        &payload,
        MessageBody::InviteJoin { invite_id: invite },
        now,
    )?;
    conn.execute(
        "UPDATE social_invites_local SET join_sent = 1 WHERE server_id = ?1 AND invite_id = ?2",
        params![text(server), text(invite)],
    )?;
    Ok(Some(message))
}

/// Whether an `installing` report is due: every ≥ 5 s or ≥ 5 % (05-social-notes §2.5).
/// Records the report when it is.
pub fn progress_due(
    conn: &Connection,
    server: Uuid,
    invite: Uuid,
    progress: f64,
    now: i64,
) -> Result<bool, StoreError> {
    let n = conn.execute(
        "UPDATE social_invites_local SET last_report_at = ?3, last_progress = ?4
         WHERE server_id = ?1 AND invite_id = ?2 AND role = 'accepted'
           AND (last_report_at IS NULL OR ?3 - last_report_at >= 5
                OR last_progress IS NULL OR abs(?4 - last_progress) >= 0.05 - 1e-9)",
        params![text(server), text(invite), now, progress],
    )?;
    Ok(n > 0)
}

/// Invites this install accepted for `package` (to follow its install).
pub fn accepted_for_package(
    conn: &Connection,
    server: Uuid,
    package: Uuid,
) -> Result<Vec<Uuid>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT invite_id FROM social_invites_local
         WHERE server_id = ?1 AND package_id = ?2 AND role = 'accepted' ORDER BY created_at",
    )?;
    let rows = stmt
        .query_map(params![text(server), text(package)], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.iter().map(|s| super::id(s)).collect()
}

/// Forgets an invite that reached a final state.
pub fn forget_invite(conn: &Connection, server: Uuid, invite: Uuid) -> Result<(), StoreError> {
    conn.execute(
        "DELETE FROM social_invites_local WHERE server_id = ?1 AND invite_id = ?2",
        params![text(server), text(invite)],
    )?;
    Ok(())
}
