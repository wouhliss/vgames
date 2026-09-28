//! Local E2EE state and the message engine (05-social §4, A4-T02).
//!
//! Synchronous functions over a `rusqlite::Connection`, called on Agent 2's database
//! thread (`Db::call`). Each state change runs in **one transaction**: an Olm ratchet step,
//! the message it produced and the "envelope processed" marker commit together, so a crash
//! never leaves a ratchet that is ahead of (or behind) the stored history.
//!
//! - Account: one Olm account per server (`ensure_account`), key uploads
//!   (`prepare_key_upload` / `mark_keys_published`).
//! - Devices: TOFU pins per device (`sync_user_devices`); a changed key is set aside and
//!   blocks sending until the user trusts it (`trust_device`).
//! - Sending: `encrypt_for` (per-device fan-out, outbound sessions from claimed keys whose
//!   signatures verify against the pinned signing key), `create_outgoing` + the outbox.
//! - Receiving: `receive` (duplicates ignored by envelope id and by
//!   `(sender device, client_message_id)`; out-of-order delivery handled by Olm).
//! - Verification: `contact_security` / `set_verified` (safety numbers).

use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;
use vgames_proto::social::{
    ClaimedKey, DeviceKeys, InboxEnvelope, MAX_ENVELOPE_CIPHERTEXT, OneTimeKeysUpload,
    OutgoingEnvelope,
};

use super::crypto::{
    self, BodyCipher, CryptoError, OlmAccount, OlmSession, PinnedDevice, SecretKey32,
    SignedDeviceKeys,
};
use super::model::{
    ContactDevice, ContactDeviceState, ContactSecurity, DeviceNotice, Message, MessageBody,
    MessageStatus, unix_rfc3339,
};
use super::payload::{self, Content, Decoded, Payload, PayloadError};
use super::secrets::{CHAT_KEY, PICKLE_KEY, SecretError, SecretStore};

mod conversations;
pub use conversations::*;

/// Give up on an outgoing message after this many failed attempts (it shows as failed and
/// can be retried by the user).
pub const OUTBOX_MAX_ATTEMPTS: u32 = 12;
/// Outbox backoff cap in seconds.
pub const OUTBOX_MAX_BACKOFF: i64 = 300;
/// Processed-envelope markers outlive the server's 30-day envelope retention.
pub const PROCESSED_RETENTION_SECS: i64 = 31 * 24 * 3600;

/// The two keychain secrets the store needs.
pub struct Keys {
    pickle: SecretKey32,
    body: BodyCipher,
}

impl Keys {
    pub fn new(pickle: SecretKey32, chat: &SecretKey32) -> Result<Self, CryptoError> {
        Ok(Self {
            pickle,
            body: BodyCipher::new(chat)?,
        })
    }

    /// Loads (or creates) both keys. Blocking: keychain access.
    pub fn from_secret_store(store: &dyn SecretStore) -> Result<Self, KeysError> {
        let pickle = store.get_or_create(PICKLE_KEY)?;
        let chat = store.get_or_create(CHAT_KEY)?;
        Ok(Self::new(pickle, &chat)?)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum KeysError {
    #[error("cannot load the social keys")]
    Secret(#[from] SecretError),
    #[error("invalid social key")]
    Crypto(#[from] CryptoError),
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error")]
    Db(#[from] rusqlite::Error),
    #[error("cryptographic failure")]
    Crypto(#[from] CryptoError),
    #[error("there is no Olm account for this server")]
    NoAccount,
    #[error("stored social data is malformed")]
    Corrupt,
    #[error("invalid message")]
    Payload(#[from] PayloadError),
}

impl From<serde_json::Error> for StoreError {
    fn from(_: serde_json::Error) -> Self {
        StoreError::Corrupt
    }
}

fn id(s: &str) -> Result<Uuid, StoreError> {
    Uuid::parse_str(s).map_err(|_| StoreError::Corrupt)
}

fn text(u: Uuid) -> String {
    u.hyphenated().to_string()
}

// ---------------------------------------------------------------------------------------
// Account
// ---------------------------------------------------------------------------------------

/// Public view of this install's account on a server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountInfo {
    pub user_id: Uuid,
    pub device_id: Option<Uuid>,
    pub identity_key: String,
    pub signing_key: String,
}

struct AccountRow {
    user_id: Uuid,
    device_id: Option<Uuid>,
    account: OlmAccount,
}

fn load_account(
    conn: &Connection,
    keys: &Keys,
    server: Uuid,
) -> Result<Option<AccountRow>, StoreError> {
    let row: Option<(String, Option<String>, String)> = conn
        .query_row(
            "SELECT user_id, device_id, account_pickle FROM social_accounts WHERE server_id = ?1",
            [text(server)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((user, device, pickle)) = row else {
        return Ok(None);
    };
    Ok(Some(AccountRow {
        user_id: id(&user)?,
        device_id: device.as_deref().map(id).transpose()?,
        account: OlmAccount::unpickle(&pickle, &keys.pickle)?,
    }))
}

fn require_account(conn: &Connection, keys: &Keys, server: Uuid) -> Result<AccountRow, StoreError> {
    load_account(conn, keys, server)?.ok_or(StoreError::NoAccount)
}

fn save_account(
    conn: &Connection,
    keys: &Keys,
    server: Uuid,
    account: &OlmAccount,
    now: i64,
) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE social_accounts SET account_pickle = ?2, updated_at = ?3 WHERE server_id = ?1",
        params![text(server), account.pickle(&keys.pickle), now],
    )?;
    Ok(())
}

fn info(row: &AccountRow) -> AccountInfo {
    AccountInfo {
        user_id: row.user_id,
        device_id: row.device_id,
        identity_key: row.account.identity_key(),
        signing_key: row.account.signing_key(),
    }
}

pub fn account_info(
    conn: &Connection,
    keys: &Keys,
    server: Uuid,
) -> Result<Option<AccountInfo>, StoreError> {
    Ok(load_account(conn, keys, server)?.as_ref().map(info))
}

/// Returns the account for `user` on `server`, creating it on first sign-in. Signing in as
/// a different user on the same server discards the previous user's social state.
pub fn ensure_account(
    conn: &mut Connection,
    keys: &Keys,
    server: Uuid,
    user: Uuid,
    now: i64,
) -> Result<AccountInfo, StoreError> {
    let tx = conn.transaction()?;
    if let Some(row) = load_account(&tx, keys, server)? {
        if row.user_id == user {
            return Ok(info(&row));
        }
        tracing::info!("another user signed in on this server; discarding local social state");
        wipe_server(&tx, server)?;
    }
    let account = OlmAccount::new();
    tx.execute(
        "INSERT INTO social_accounts (server_id, user_id, device_id, account_pickle, created_at, updated_at)
         VALUES (?1, ?2, NULL, ?3, ?4, ?4)",
        params![text(server), text(user), account.pickle(&keys.pickle), now],
    )?;
    let row = AccountRow {
        user_id: user,
        device_id: None,
        account,
    };
    let out = info(&row);
    tx.commit()?;
    Ok(out)
}

/// Replaces the Olm account of `server` with a fresh one (the server refused the old keys:
/// the device was revoked from another install). Sessions made with the old account are
/// dropped; pins, verification and history stay. The new account needs registering.
pub fn replace_account(
    conn: &mut Connection,
    keys: &Keys,
    server: Uuid,
    now: i64,
) -> Result<AccountInfo, StoreError> {
    let tx = conn.transaction()?;
    let old = require_account(&tx, keys, server)?;
    let account = OlmAccount::new();
    tx.execute(
        "UPDATE social_accounts SET device_id = NULL, account_pickle = ?2, updated_at = ?3 WHERE server_id = ?1",
        params![text(server), account.pickle(&keys.pickle), now],
    )?;
    tx.execute(
        "DELETE FROM social_olm_sessions WHERE server_id = ?1",
        [text(server)],
    )?;
    let row = AccountRow {
        user_id: old.user_id,
        device_id: None,
        account,
    };
    let out = info(&row);
    tx.commit()?;
    Ok(out)
}

/// Deletes every social row of `server` (sign-out of a user, server removed by hand).
pub fn wipe_server(conn: &Connection, server: Uuid) -> Result<(), StoreError> {
    for table in [
        "social_accounts",
        "social_olm_sessions",
        "social_devices",
        "social_contacts",
        "social_conversations",
        "social_messages",
        "social_outbox",
        "social_processed_envelopes",
        "social_blocks",
    ] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE server_id = ?1"),
            [text(server)],
        )?;
    }
    Ok(())
}

/// Keys and self-signature for `POST /v1/devices`.
pub fn signed_device_keys(
    conn: &Connection,
    keys: &Keys,
    server: Uuid,
) -> Result<SignedDeviceKeys, StoreError> {
    let row = require_account(conn, keys, server)?;
    Ok(row.account.signed_device_keys(row.user_id, server))
}

/// Records the device id the server assigned.
pub fn set_device_id(
    conn: &Connection,
    server: Uuid,
    device: Uuid,
    now: i64,
) -> Result<(), StoreError> {
    let n = conn.execute(
        "UPDATE social_accounts SET device_id = ?2, updated_at = ?3 WHERE server_id = ?1",
        params![text(server), text(device), now],
    )?;
    if n == 0 {
        return Err(StoreError::NoAccount);
    }
    Ok(())
}

/// Generates `new_one_time_keys` more keys (and rotates the fallback key if asked) and
/// returns every key not yet published. Keys from a failed upload are returned again.
pub fn prepare_key_upload(
    conn: &mut Connection,
    keys: &Keys,
    server: Uuid,
    new_one_time_keys: usize,
    rotate_fallback: bool,
    now: i64,
) -> Result<OneTimeKeysUpload, StoreError> {
    let tx = conn.transaction()?;
    let mut row = require_account(&tx, keys, server)?;
    if new_one_time_keys > 0 {
        row.account.generate_one_time_keys(new_one_time_keys);
    }
    if rotate_fallback {
        row.account.generate_fallback_key();
    }
    save_account(&tx, keys, server, &row.account, now)?;
    let (one_time_keys, fallback_key) = row.account.unpublished_keys();
    tx.commit()?;
    Ok(OneTimeKeysUpload {
        one_time_keys,
        fallback_key,
    })
}

/// Keys to upload so the server holds [`crypto::INITIAL_ONE_TIME_KEYS`] unclaimed one-time
/// keys again (`available` is the server's count), plus a fallback key when the server has
/// none. Keys of an earlier failed upload are sent again first; the total never exceeds
/// the server's cap.
pub fn prepare_top_up(
    conn: &mut Connection,
    keys: &Keys,
    server: Uuid,
    available: usize,
    rotate_fallback: bool,
    now: i64,
) -> Result<OneTimeKeysUpload, StoreError> {
    let pending = {
        let row = require_account(conn, keys, server)?;
        row.account.unpublished_keys().0.len()
    };
    let have = available.saturating_add(pending);
    let new = crypto::INITIAL_ONE_TIME_KEYS
        .saturating_sub(have)
        .min(vgames_proto::social::MAX_UNCLAIMED_ONE_TIME_KEYS.saturating_sub(have));
    prepare_key_upload(conn, keys, server, new, rotate_fallback, now)
}

/// Call after the server stored the keys from [`prepare_key_upload`].
pub fn mark_keys_published(
    conn: &mut Connection,
    keys: &Keys,
    server: Uuid,
    now: i64,
) -> Result<(), StoreError> {
    let tx = conn.transaction()?;
    let mut row = require_account(&tx, keys, server)?;
    row.account.mark_keys_as_published();
    save_account(&tx, keys, server, &row.account, now)?;
    tx.commit()?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Devices (TOFU pins)
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinState {
    Trusted,
    KeyChanged,
    Revoked,
}

impl PinState {
    fn parse(s: &str) -> Result<Self, StoreError> {
        Ok(match s {
            "trusted" => Self::Trusted,
            "key_changed" => Self::KeyChanged,
            "revoked" => Self::Revoked,
            _ => return Err(StoreError::Corrupt),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedRow {
    pub device: PinnedDevice,
    pub display_name: Option<String>,
    pub state: PinState,
    pub first_seen_at: i64,
}

/// What changed in a user's device list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceChanges {
    /// The user had no pinned device before: nothing is "new", nothing to announce.
    pub first_sight: bool,
    pub new: Vec<(Uuid, Option<String>)>,
    pub key_changed: Vec<Uuid>,
    pub revoked: Vec<Uuid>,
    /// Devices whose self-signature failed (never pinned).
    pub rejected: Vec<Uuid>,
}

impl DeviceChanges {
    pub fn is_empty(&self) -> bool {
        self.new.is_empty() && self.key_changed.is_empty() && self.revoked.is_empty()
    }

    /// Notices to show in conversations with `user`.
    pub fn notices(&self, user: Uuid) -> Vec<DeviceNotice> {
        if self.first_sight {
            return Vec::new();
        }
        let mut out: Vec<DeviceNotice> = self
            .new
            .iter()
            .map(|(device, name)| DeviceNotice::NewDevice {
                user_id: user,
                device_id: *device,
                device_name: name.clone().unwrap_or_default(),
            })
            .collect();
        out.extend(self.key_changed.iter().map(|d| DeviceNotice::KeyChanged {
            user_id: user,
            device_id: *d,
        }));
        out.extend(self.revoked.iter().map(|d| DeviceNotice::DeviceRevoked {
            user_id: user,
            device_id: *d,
        }));
        out
    }
}

type PinnedTuple = (String, String, Option<String>, String, String, String, i64);

fn map_pinned(r: &rusqlite::Row<'_>) -> rusqlite::Result<PinnedTuple> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
    ))
}

fn pinned_from(t: PinnedTuple) -> Result<PinnedRow, StoreError> {
    let (device, user, name, identity, signing, state, first_seen) = t;
    Ok(PinnedRow {
        device: PinnedDevice {
            device_id: id(&device)?,
            user_id: id(&user)?,
            identity_key: identity,
            signing_key: signing,
        },
        display_name: name,
        state: PinState::parse(&state)?,
        first_seen_at: first_seen,
    })
}

const PINNED_COLUMNS: &str =
    "device_id, user_id, display_name, identity_key, signing_key, state, first_seen_at";

/// Every pinned device of `user` (any state).
pub fn pinned_devices(
    conn: &Connection,
    server: Uuid,
    user: Uuid,
) -> Result<Vec<PinnedRow>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {PINNED_COLUMNS} FROM social_devices WHERE server_id = ?1 AND user_id = ?2 ORDER BY first_seen_at, device_id"
    ))?;
    let rows = stmt
        .query_map(params![text(server), text(user)], map_pinned)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(pinned_from).collect()
}

fn pinned_device(
    conn: &Connection,
    server: Uuid,
    device: Uuid,
) -> Result<Option<PinnedRow>, StoreError> {
    conn.query_row(
        &format!(
            "SELECT {PINNED_COLUMNS} FROM social_devices WHERE server_id = ?1 AND device_id = ?2"
        ),
        params![text(server), text(device)],
        map_pinned,
    )
    .optional()?
    .map(pinned_from)
    .transpose()
}

/// Reconciles the pins for `user` with the device list the server returned
/// (`GET /v1/users/{id}/devices`). New devices are pinned on first sight; a known device id
/// with different keys becomes `key_changed`; pinned devices missing from the list are
/// revoked. This install's own device is ignored.
pub fn sync_user_devices(
    conn: &mut Connection,
    keys: &Keys,
    server: Uuid,
    user: Uuid,
    devices: &[DeviceKeys],
    now: i64,
) -> Result<DeviceChanges, StoreError> {
    let tx = conn.transaction()?;
    let me = require_account(&tx, keys, server)?;
    let existing = pinned_devices(&tx, server, user)?;
    let mut changes = DeviceChanges {
        first_sight: existing.is_empty(),
        ..DeviceChanges::default()
    };
    let mut seen = Vec::with_capacity(devices.len());
    for d in devices {
        if Some(d.device_id) == me.device_id {
            continue;
        }
        let pinned = match crypto::verify_device_keys(d, user, server) {
            Ok(p) => p,
            Err(error) => {
                tracing::warn!(device_id = %d.device_id, %error, "ignoring a device with a bad self-signature");
                changes.rejected.push(d.device_id);
                continue;
            }
        };
        seen.push(d.device_id);
        let name = d.display_name.clone();
        match existing.iter().find(|e| e.device.device_id == d.device_id) {
            None => {
                // A device id pinned under another user is never re-pinned here.
                if pinned_device(&tx, server, d.device_id)?.is_some() {
                    changes.rejected.push(d.device_id);
                    continue;
                }
                tx.execute(
                    "INSERT INTO social_devices (server_id, device_id, user_id, display_name, identity_key, signing_key, state, first_seen_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'trusted', ?7, ?7)",
                    params![text(server), text(d.device_id), text(user), name, pinned.identity_key, pinned.signing_key, now],
                )?;
                changes.new.push((d.device_id, name));
            }
            Some(e) if e.state == PinState::Revoked => {}
            Some(e) => {
                let same = e.device.identity_key == pinned.identity_key
                    && e.device.signing_key == pinned.signing_key;
                if same {
                    if e.state == PinState::KeyChanged {
                        // The server went back to the pinned keys.
                        tx.execute(
                            "UPDATE social_devices SET state = 'trusted', pending_identity_key = NULL, pending_signing_key = NULL, updated_at = ?3
                             WHERE server_id = ?1 AND device_id = ?2",
                            params![text(server), text(d.device_id), now],
                        )?;
                    }
                    tx.execute(
                        "UPDATE social_devices SET display_name = ?3 WHERE server_id = ?1 AND device_id = ?2",
                        params![text(server), text(d.device_id), name],
                    )?;
                    continue;
                }
                let n = tx.execute(
                    "UPDATE social_devices SET state = 'key_changed', pending_identity_key = ?3, pending_signing_key = ?4, updated_at = ?5
                     WHERE server_id = ?1 AND device_id = ?2
                       AND NOT (state = 'key_changed' AND pending_identity_key = ?3 AND pending_signing_key = ?4)",
                    params![text(server), text(d.device_id), pinned.identity_key, pinned.signing_key, now],
                )?;
                if n > 0 {
                    tracing::warn!(device_id = %d.device_id, "a contact device presented a different key; sending to it is blocked");
                    changes.key_changed.push(d.device_id);
                }
            }
        }
    }
    for e in &existing {
        if e.state != PinState::Revoked && !seen.contains(&e.device.device_id) {
            revoke_pin(&tx, server, e.device.device_id, now)?;
            changes.revoked.push(e.device.device_id);
        }
    }
    tx.commit()?;
    Ok(changes)
}

fn revoke_pin(conn: &Connection, server: Uuid, device: Uuid, now: i64) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE social_devices SET state = 'revoked', pending_identity_key = NULL, pending_signing_key = NULL, updated_at = ?3
         WHERE server_id = ?1 AND device_id = ?2",
        params![text(server), text(device), now],
    )?;
    conn.execute(
        "DELETE FROM social_olm_sessions WHERE server_id = ?1 AND peer_device_id = ?2",
        params![text(server), text(device)],
    )?;
    Ok(())
}

/// Handles `device.revoked`: the device stops receiving and its sessions are dropped.
/// Returns the device's user when it was pinned.
pub fn mark_device_revoked(
    conn: &Connection,
    server: Uuid,
    device: Uuid,
    now: i64,
) -> Result<Option<Uuid>, StoreError> {
    let Some(row) = pinned_device(conn, server, device)? else {
        return Ok(None);
    };
    if row.state != PinState::Revoked {
        revoke_pin(conn, server, device, now)?;
    }
    Ok(Some(row.device.user_id))
}

/// Accepts the changed key of `device` (after the user compared safety numbers). Old
/// sessions are dropped; the next message starts a new one.
pub fn trust_device(
    conn: &mut Connection,
    server: Uuid,
    user: Uuid,
    device: Uuid,
    now: i64,
) -> Result<bool, StoreError> {
    let tx = conn.transaction()?;
    let n = tx.execute(
        "UPDATE social_devices
         SET identity_key = pending_identity_key, signing_key = pending_signing_key,
             pending_identity_key = NULL, pending_signing_key = NULL, state = 'trusted', updated_at = ?4
         WHERE server_id = ?1 AND user_id = ?2 AND device_id = ?3 AND state = 'key_changed'",
        params![text(server), text(user), text(device), now],
    )?;
    if n > 0 {
        tx.execute(
            "DELETE FROM social_olm_sessions WHERE server_id = ?1 AND peer_device_id = ?2",
            params![text(server), text(device)],
        )?;
    }
    tx.commit()?;
    Ok(n > 0)
}

// ---------------------------------------------------------------------------------------
// Sessions and sending
// ---------------------------------------------------------------------------------------

struct SessionRow {
    session_id: String,
    session: OlmSession,
}

fn sessions_for(
    conn: &Connection,
    keys: &Keys,
    server: Uuid,
    device: Uuid,
) -> Result<Vec<SessionRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT session_id, pickle FROM social_olm_sessions WHERE server_id = ?1 AND peer_device_id = ?2
         ORDER BY last_used_at DESC, created_at DESC",
    )?;
    let rows = stmt
        .query_map(params![text(server), text(device)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(session_id, pickle)| {
            Ok(SessionRow {
                session_id,
                session: OlmSession::unpickle(&pickle, &keys.pickle)?,
            })
        })
        .collect()
}

fn save_session(
    conn: &Connection,
    keys: &Keys,
    server: Uuid,
    peer: &PinnedDevice,
    session: &OlmSession,
    now: i64,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO social_olm_sessions (server_id, session_id, peer_device_id, peer_identity_key, pickle, created_at, last_used_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
         ON CONFLICT (server_id, session_id) DO UPDATE SET pickle = excluded.pickle, last_used_at = excluded.last_used_at",
        params![
            text(server),
            session.session_id(),
            text(peer.device_id),
            peer.identity_key,
            session.pickle(&keys.pickle),
            now
        ],
    )?;
    Ok(())
}

/// Where a message to `users` goes: their pinned, non-revoked devices (this install's own
/// device excluded), split into sendable ones and ones whose key changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Targets {
    /// `(user, device)` pairs that can receive.
    pub trusted: Vec<(Uuid, Uuid)>,
    /// `(user, device)` pairs whose key changed: sending is blocked until the user trusts them.
    pub key_changed: Vec<(Uuid, Uuid)>,
    /// Users with no pinned device at all (their device list was never fetched).
    pub never_synced: Vec<Uuid>,
}

pub fn targets(
    conn: &Connection,
    keys: &Keys,
    server: Uuid,
    users: &[Uuid],
) -> Result<Targets, StoreError> {
    let me = require_account(conn, keys, server)?;
    let mut out = Targets::default();
    for user in users {
        let pins = pinned_devices(conn, server, *user)?;
        if pins.is_empty() {
            out.never_synced.push(*user);
        }
        for p in pins {
            if Some(p.device.device_id) == me.device_id {
                continue;
            }
            match p.state {
                PinState::Trusted => out.trusted.push((*user, p.device.device_id)),
                PinState::KeyChanged => out.key_changed.push((*user, p.device.device_id)),
                PinState::Revoked => {}
            }
        }
    }
    Ok(out)
}

/// Devices (of `candidates`) with no Olm session yet: claim a key for each before sending.
pub fn devices_without_session(
    conn: &Connection,
    server: Uuid,
    candidates: &[Uuid],
) -> Result<Vec<Uuid>, StoreError> {
    let mut out = Vec::new();
    for d in candidates {
        let has: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM social_olm_sessions WHERE server_id = ?1 AND peer_device_id = ?2)",
            params![text(server), text(*d)],
            |r| r.get(0),
        )?;
        if !has {
            out.push(*d);
        }
    }
    Ok(out)
}

/// Result of a per-device fan-out.
#[derive(Debug, Default)]
pub struct Encrypted {
    pub envelopes: Vec<OutgoingEnvelope>,
    /// No session and no (valid) claimed key: claim again or skip.
    pub missing: Vec<Uuid>,
    /// Key changed since it was pinned: sending blocked until the user trusts it.
    pub blocked: Vec<(Uuid, Uuid)>,
    /// Not pinned (fetch the user's devices first).
    pub unknown: Vec<Uuid>,
}

/// Encrypts `plaintext` once per device in `devices` (05-social §4.1 step 3).
pub fn encrypt_for(
    conn: &mut Connection,
    keys: &Keys,
    server: Uuid,
    devices: &[Uuid],
    claimed: &[ClaimedKey],
    plaintext: &[u8],
    now: i64,
) -> Result<Encrypted, StoreError> {
    let tx = conn.transaction()?;
    let me = require_account(&tx, keys, server)?;
    let mut out = Encrypted::default();
    for device in devices {
        if Some(*device) == me.device_id {
            continue;
        }
        let Some(pin) = pinned_device(&tx, server, *device)? else {
            out.unknown.push(*device);
            continue;
        };
        match pin.state {
            PinState::Revoked => continue,
            PinState::KeyChanged => {
                out.blocked.push((pin.device.user_id, *device));
                continue;
            }
            PinState::Trusted => {}
        }
        let mut session = match sessions_for(&tx, keys, server, *device)?.into_iter().next() {
            Some(s) => s.session,
            None => {
                let Some(key) = claimed.iter().find(|k| k.device_id == *device) else {
                    out.missing.push(*device);
                    continue;
                };
                match me.account.create_outbound_session(&pin.device, key) {
                    Ok(s) => s,
                    Err(error) => {
                        tracing::warn!(device_id = %device, %error, "claimed key rejected");
                        out.missing.push(*device);
                        continue;
                    }
                }
            }
        };
        let (message_type, bytes) = session.encrypt(plaintext)?;
        save_session(&tx, keys, server, &pin.device, &session, now)?;
        out.envelopes.push(OutgoingEnvelope {
            recipient_device_id: *device,
            olm_message_type: message_type,
            ciphertext: crypto::wire_b64(&bytes),
        });
    }
    tx.commit()?;
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Receiving
// ---------------------------------------------------------------------------------------

/// Outcome of processing one inbox envelope.
#[derive(Clone, Debug, PartialEq)]
pub enum Received {
    /// A new message was stored (text, device copy of our own message, or unsupported).
    Message(Message),
    /// `invite.join` from the invite's sender. The secret is only returned, never stored.
    InviteJoin {
        message: Message,
        invite_id: Uuid,
        join_secret: Option<String>,
        sender_device_id: Uuid,
    },
    /// `receipt.read`.
    Receipt {
        conversation_id: Uuid,
        sender_user_id: Uuid,
        up_to: Uuid,
    },
    /// Already processed (redelivery or resend): acknowledge and move on.
    Duplicate,
}

#[derive(Debug, thiserror::Error)]
pub enum ReceiveError {
    #[error("the sender device is not pinned")]
    UnknownSender { user_id: Uuid, device_id: Uuid },
    #[error("the sender key does not match the pinned key")]
    KeyMismatch { user_id: Uuid, device_id: Uuid },
    #[error("the envelope cannot be decrypted")]
    Undecryptable(#[source] CryptoError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<rusqlite::Error> for ReceiveError {
    fn from(e: rusqlite::Error) -> Self {
        ReceiveError::Store(StoreError::Db(e))
    }
}

fn is_processed(conn: &Connection, server: Uuid, envelope: Uuid) -> Result<bool, StoreError> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM social_processed_envelopes WHERE server_id = ?1 AND envelope_id = ?2)",
        params![text(server), text(envelope)],
        |r| r.get(0),
    )?)
}

/// Marks an envelope processed without storing anything (for envelopes the caller decided
/// to drop, e.g. from a revoked device).
pub fn mark_envelope_processed(
    conn: &Connection,
    server: Uuid,
    envelope: Uuid,
    now: i64,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT OR IGNORE INTO social_processed_envelopes (server_id, envelope_id, processed_at) VALUES (?1, ?2, ?3)",
        params![text(server), text(envelope), now],
    )?;
    Ok(())
}

/// Decrypts, de-duplicates and stores one envelope for this device. `me` is the local user.
pub fn receive(
    conn: &mut Connection,
    keys: &Keys,
    server: Uuid,
    me: Uuid,
    env: &InboxEnvelope,
    now: i64,
) -> Result<Received, ReceiveError> {
    let tx = conn.transaction()?;
    if is_processed(&tx, server, env.id)? {
        return Ok(Received::Duplicate);
    }
    let sender = match pinned_device(&tx, server, env.sender_device_id)? {
        Some(p) if p.device.user_id == env.sender_user_id && p.state != PinState::Revoked => p,
        _ => {
            return Err(ReceiveError::UnknownSender {
                user_id: env.sender_user_id,
                device_id: env.sender_device_id,
            });
        }
    };
    if sender.device.identity_key != env.sender_identity_key {
        return Err(ReceiveError::KeyMismatch {
            user_id: env.sender_user_id,
            device_id: env.sender_device_id,
        });
    }
    // Bound the work before decoding: the server caps ciphertexts at 64 KiB (05-social §4.4).
    if env.ciphertext.len() > MAX_ENVELOPE_CIPHERTEXT.div_ceil(3) * 4 {
        return Err(ReceiveError::Undecryptable(CryptoError::BadMessage));
    }
    let bytes = crypto::wire_b64_decode(&env.ciphertext).map_err(ReceiveError::Undecryptable)?;
    if bytes.len() > MAX_ENVELOPE_CIPHERTEXT {
        return Err(ReceiveError::Undecryptable(CryptoError::BadMessage));
    }
    let message =
        crypto::parse_message(env.olm_message_type, &bytes).map_err(ReceiveError::Undecryptable)?;

    let sessions = sessions_for(&tx, keys, server, env.sender_device_id)?;
    let (session, plaintext) = match crypto::pre_key_session_id(&message) {
        Some(pre_key_id) => match sessions.into_iter().find(|s| s.session_id == pre_key_id) {
            Some(mut s) => {
                let p = s
                    .session
                    .decrypt(&message)
                    .map_err(ReceiveError::Undecryptable)?;
                (s.session, p)
            }
            None => {
                let mut account = require_account(&tx, keys, server)?;
                let (session, p) = account
                    .account
                    .create_inbound_session(&sender.device.identity_key, &message)
                    .map_err(ReceiveError::Undecryptable)?;
                save_account(&tx, keys, server, &account.account, now)?;
                (session, p)
            }
        },
        None => {
            let mut found = None;
            for mut s in sessions {
                if let Ok(p) = s.session.decrypt(&message) {
                    found = Some((s.session, p));
                    break;
                }
            }
            found.ok_or(ReceiveError::Undecryptable(CryptoError::NoSession))?
        }
    };
    save_session(&tx, keys, server, &sender.device, &session, now)?;

    let decoded = payload::decode(&plaintext).map_err(StoreError::from)?;
    let mine = env.sender_user_id == me;
    let (client_message_id, sent_at, body, extra) = match decoded {
        Decoded::Known(p) if p.conversation_id == env.conversation_id => {
            let sent_at = p.sent_at.unix_timestamp();
            match p.content {
                Content::Text { body } => (
                    p.client_message_id,
                    sent_at,
                    MessageBody::Text { text: body },
                    None,
                ),
                Content::InviteJoin {
                    invite_id,
                    join_secret,
                } => (
                    p.client_message_id,
                    sent_at,
                    MessageBody::InviteJoin { invite_id },
                    Some((invite_id, join_secret)),
                ),
                Content::ReceiptRead { up_to } => {
                    mark_envelope_processed(&tx, server, env.id, now)?;
                    tx.commit()?;
                    return Ok(Received::Receipt {
                        conversation_id: env.conversation_id,
                        sender_user_id: env.sender_user_id,
                        up_to,
                    });
                }
            }
        }
        Decoded::Known(p) => (
            p.client_message_id,
            p.sent_at.unix_timestamp(),
            MessageBody::Unsupported,
            None,
        ),
        Decoded::Unsupported {
            client_message_id,
            sent_at,
            ..
        } => (
            client_message_id,
            sent_at.unix_timestamp(),
            MessageBody::Unsupported,
            None,
        ),
    };

    let stored = insert_message(
        &tx,
        keys,
        server,
        NewMessage {
            conversation_id: env.conversation_id,
            sender_user_id: env.sender_user_id,
            sender_device_id: env.sender_device_id,
            client_message_id,
            mine,
            status: if mine {
                MessageStatus::Sent
            } else {
                MessageStatus::Received
            },
            body,
            sent_at,
            received_at: Some(now),
        },
    )?;
    mark_envelope_processed(&tx, server, env.id, now)?;
    tx.commit()?;
    let Some(message) = stored else {
        return Ok(Received::Duplicate);
    };
    Ok(match extra {
        Some((invite_id, join_secret)) => Received::InviteJoin {
            message,
            invite_id,
            join_secret,
            sender_device_id: env.sender_device_id,
        },
        None => Received::Message(message),
    })
}

// ---------------------------------------------------------------------------------------
// Messages and the outbox
// ---------------------------------------------------------------------------------------

struct NewMessage {
    conversation_id: Uuid,
    sender_user_id: Uuid,
    sender_device_id: Uuid,
    client_message_id: Uuid,
    mine: bool,
    status: MessageStatus,
    body: MessageBody,
    sent_at: i64,
    received_at: Option<i64>,
}

fn body_aad(server: Uuid, message: Uuid) -> Vec<u8> {
    let mut aad = b"vgames message body v1\0".to_vec();
    aad.extend_from_slice(server.as_bytes());
    aad.extend_from_slice(message.as_bytes());
    aad
}

fn outbox_aad(server: Uuid, client_message_id: Uuid) -> Vec<u8> {
    let mut aad = b"vgames outbox payload v1\0".to_vec();
    aad.extend_from_slice(server.as_bytes());
    aad.extend_from_slice(client_message_id.as_bytes());
    aad
}

/// Inserts a message; `None` if `(sender device, client_message_id)` is already stored.
fn insert_message(
    conn: &Connection,
    keys: &Keys,
    server: Uuid,
    m: NewMessage,
) -> Result<Option<Message>, StoreError> {
    let message_id = Uuid::now_v7();
    let json = serde_json::to_vec(&m.body)?;
    let (nonce, sealed) = keys.body.seal(&body_aad(server, message_id), &json)?;
    let n = conn.execute(
        "INSERT OR IGNORE INTO social_messages
           (server_id, id, conversation_id, sender_user_id, sender_device_id, client_message_id, direction, status, body_nonce, body, sent_at, received_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            text(server),
            text(message_id),
            text(m.conversation_id),
            text(m.sender_user_id),
            text(m.sender_device_id),
            text(m.client_message_id),
            if m.mine { "out" } else { "in" },
            m.status.as_str(),
            &nonce[..],
            sealed,
            m.sent_at,
            m.received_at
        ],
    )?;
    if n == 0 {
        return Ok(None);
    }
    conn.execute(
        "UPDATE social_conversations SET last_activity_at = max(last_activity_at, ?3) WHERE server_id = ?1 AND id = ?2",
        params![text(server), text(m.conversation_id), m.received_at.unwrap_or(m.sent_at)],
    )?;
    Ok(Some(Message {
        id: message_id,
        conversation_id: m.conversation_id,
        sender_user_id: m.sender_user_id,
        mine: m.mine,
        body: m.body,
        sent_at: unix_rfc3339(m.sent_at),
        received_at: m.received_at.map(unix_rfc3339),
        status: m.status,
    }))
}

const MESSAGE_COLUMNS: &str = "id, conversation_id, sender_user_id, direction, status, body_nonce, body, sent_at, received_at";

type MessageTuple = (
    String,
    String,
    String,
    String,
    String,
    Vec<u8>,
    Vec<u8>,
    i64,
    Option<i64>,
);

fn map_message(r: &rusqlite::Row<'_>) -> rusqlite::Result<MessageTuple> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
    ))
}

fn message_from(keys: &Keys, server: Uuid, t: MessageTuple) -> Result<Message, StoreError> {
    let (mid, conv, sender, direction, status, nonce, sealed, sent_at, received_at) = t;
    let message_id = id(&mid)?;
    let json = keys
        .body
        .open(&body_aad(server, message_id), &nonce, &sealed)?;
    Ok(Message {
        id: message_id,
        conversation_id: id(&conv)?,
        sender_user_id: id(&sender)?,
        mine: direction == "out",
        body: serde_json::from_slice(&json)?,
        sent_at: unix_rfc3339(sent_at),
        received_at: received_at.map(unix_rfc3339),
        status: MessageStatus::parse(&status),
    })
}

/// One message by id.
pub fn get_message(
    conn: &Connection,
    keys: &Keys,
    server: Uuid,
    message: Uuid,
) -> Result<Option<Message>, StoreError> {
    conn.query_row(
        &format!("SELECT {MESSAGE_COLUMNS} FROM social_messages WHERE server_id = ?1 AND id = ?2"),
        params![text(server), text(message)],
        map_message,
    )
    .optional()?
    .map(|t| message_from(keys, server, t))
    .transpose()
}

/// Messages of a conversation in arrival order, `limit` at most, before `before` if given.
pub fn list_messages(
    conn: &Connection,
    keys: &Keys,
    server: Uuid,
    conversation: Uuid,
    before: Option<Uuid>,
    limit: u32,
) -> Result<Vec<Message>, StoreError> {
    let before_seq: i64 = match before {
        None => i64::MAX,
        Some(b) => conn
            .query_row(
                "SELECT seq FROM social_messages WHERE server_id = ?1 AND id = ?2",
                params![text(server), text(b)],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(i64::MAX),
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT {MESSAGE_COLUMNS} FROM social_messages
         WHERE server_id = ?1 AND conversation_id = ?2 AND seq < ?3 ORDER BY seq DESC LIMIT ?4"
    ))?;
    let rows = stmt
        .query_map(
            params![
                text(server),
                text(conversation),
                before_seq,
                i64::from(limit)
            ],
            map_message,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = rows
        .into_iter()
        .map(|t| message_from(keys, server, t))
        .collect::<Result<Vec<_>, _>>()?;
    out.reverse();
    Ok(out)
}

/// Stores a device notice ("Sam signed in on a new device") in a conversation.
pub fn insert_notice(
    conn: &Connection,
    keys: &Keys,
    server: Uuid,
    conversation: Uuid,
    notice: DeviceNotice,
    now: i64,
) -> Result<Option<Message>, StoreError> {
    let (user, device) = match &notice {
        DeviceNotice::NewDevice {
            user_id, device_id, ..
        }
        | DeviceNotice::KeyChanged { user_id, device_id }
        | DeviceNotice::DeviceRevoked { user_id, device_id } => (*user_id, *device_id),
    };
    insert_message(
        conn,
        keys,
        server,
        NewMessage {
            conversation_id: conversation,
            sender_user_id: user,
            sender_device_id: device,
            client_message_id: Uuid::now_v7(),
            mine: false,
            status: MessageStatus::Received,
            body: MessageBody::Notice { notice },
            sent_at: now,
            received_at: Some(now),
        },
    )
}

/// Stores an outgoing message (`pending`) and queues its payload for sending.
pub fn create_outgoing(
    conn: &mut Connection,
    keys: &Keys,
    server: Uuid,
    payload: &Payload,
    body: MessageBody,
    now: i64,
) -> Result<Message, StoreError> {
    let tx = conn.transaction()?;
    let me = require_account(&tx, keys, server)?;
    let device = me.device_id.ok_or(StoreError::NoAccount)?;
    let plaintext = payload.encode()?;
    let message = insert_message(
        &tx,
        keys,
        server,
        NewMessage {
            conversation_id: payload.conversation_id,
            sender_user_id: me.user_id,
            sender_device_id: device,
            client_message_id: payload.client_message_id,
            mine: true,
            status: MessageStatus::Pending,
            body,
            sent_at: payload.sent_at.unix_timestamp(),
            received_at: None,
        },
    )?
    .ok_or(StoreError::Corrupt)?;
    let (nonce, sealed) = keys
        .body
        .seal(&outbox_aad(server, payload.client_message_id), &plaintext)?;
    tx.execute(
        "INSERT INTO social_outbox (server_id, client_message_id, message_id, conversation_id, payload_nonce, payload, attempts, next_attempt_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?7)",
        params![
            text(server),
            text(payload.client_message_id),
            text(message.id),
            text(payload.conversation_id),
            &nonce[..],
            sealed,
            now
        ],
    )?;
    tx.commit()?;
    Ok(message)
}

/// A queued outgoing message, decrypted for sending.
#[derive(Clone, Debug)]
pub struct OutboxItem {
    pub client_message_id: Uuid,
    pub message_id: Uuid,
    pub conversation_id: Uuid,
    pub plaintext: Vec<u8>,
    pub attempts: u32,
}

/// Outbox entries due at `now`, oldest first.
pub fn outbox_due(
    conn: &Connection,
    keys: &Keys,
    server: Uuid,
    now: i64,
    limit: u32,
) -> Result<Vec<OutboxItem>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT client_message_id, message_id, conversation_id, payload_nonce, payload, attempts
         FROM social_outbox WHERE server_id = ?1 AND next_attempt_at <= ?2 ORDER BY created_at, client_message_id LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(params![text(server), now, i64::from(limit)], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Vec<u8>>(3)?,
                r.get::<_, Vec<u8>>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(cmid, mid, conv, nonce, sealed, attempts)| {
            let client_message_id = id(&cmid)?;
            Ok(OutboxItem {
                plaintext: keys.body.open(
                    &outbox_aad(server, client_message_id),
                    &nonce,
                    &sealed,
                )?,
                client_message_id,
                message_id: id(&mid)?,
                conversation_id: id(&conv)?,
                attempts: u32::try_from(attempts).unwrap_or(u32::MAX),
            })
        })
        .collect()
}

/// When the next outbox entry becomes due (for the single retry timer; no polling).
pub fn outbox_next_due(conn: &Connection, server: Uuid) -> Result<Option<i64>, StoreError> {
    Ok(conn.query_row(
        "SELECT min(next_attempt_at) FROM social_outbox WHERE server_id = ?1 AND next_attempt_at < ?2",
        params![text(server), i64::MAX],
        |r| r.get(0),
    )?)
}

fn set_status(
    conn: &Connection,
    server: Uuid,
    message: Uuid,
    status: MessageStatus,
) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE social_messages SET status = ?3 WHERE server_id = ?1 AND id = ?2",
        params![text(server), text(message), status.as_str()],
    )?;
    Ok(())
}

/// The server accepted the message: drop it from the outbox and mark it sent.
pub fn outbox_sent(
    conn: &mut Connection,
    server: Uuid,
    client_message_id: Uuid,
) -> Result<Option<(Uuid, Uuid)>, StoreError> {
    let tx = conn.transaction()?;
    let row: Option<(String, String)> = tx
        .query_row(
            "DELETE FROM social_outbox WHERE server_id = ?1 AND client_message_id = ?2 RETURNING message_id, conversation_id",
            params![text(server), text(client_message_id)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let out = match row {
        Some((m, c)) => {
            let (m, c) = (id(&m)?, id(&c)?);
            set_status(&tx, server, m, MessageStatus::Sent)?;
            Some((m, c))
        }
        None => None,
    };
    tx.commit()?;
    Ok(out)
}

/// Seconds to wait before attempt `attempts + 1`: 2, 4, 8 … capped at 5 minutes.
pub fn outbox_backoff(attempts: u32) -> i64 {
    let exp = attempts.min(16);
    (2i64 << exp).min(OUTBOX_MAX_BACKOFF)
}

/// What happened to a message whose send failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryOutcome {
    RetryAt(i64),
    /// Out of attempts (or a permanent error): the message is `failed` until the user retries.
    Failed {
        message_id: Uuid,
        conversation_id: Uuid,
    },
    Gone,
}

/// Records a failed attempt; schedules a retry with backoff, or fails the message when
/// `permanent` or out of attempts.
pub fn outbox_attempt_failed(
    conn: &mut Connection,
    server: Uuid,
    client_message_id: Uuid,
    now: i64,
    error: &str,
    permanent: bool,
) -> Result<RetryOutcome, StoreError> {
    let tx = conn.transaction()?;
    let row: Option<(String, String, i64)> = tx
        .query_row(
            "SELECT message_id, conversation_id, attempts FROM social_outbox WHERE server_id = ?1 AND client_message_id = ?2",
            params![text(server), text(client_message_id)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((m, c, attempts)) = row else {
        return Ok(RetryOutcome::Gone);
    };
    let attempts = u32::try_from(attempts)
        .unwrap_or(u32::MAX)
        .saturating_add(1);
    let error: String = error.chars().take(200).collect();
    let outcome = if permanent || attempts >= OUTBOX_MAX_ATTEMPTS {
        tx.execute(
            "UPDATE social_outbox SET attempts = ?3, last_error = ?4, next_attempt_at = ?5 WHERE server_id = ?1 AND client_message_id = ?2",
            params![text(server), text(client_message_id), i64::from(attempts), error, i64::MAX],
        )?;
        let (message_id, conversation_id) = (id(&m)?, id(&c)?);
        set_status(&tx, server, message_id, MessageStatus::Failed)?;
        RetryOutcome::Failed {
            message_id,
            conversation_id,
        }
    } else {
        let at = now.saturating_add(outbox_backoff(attempts));
        tx.execute(
            "UPDATE social_outbox SET attempts = ?3, last_error = ?4, next_attempt_at = ?5 WHERE server_id = ?1 AND client_message_id = ?2",
            params![text(server), text(client_message_id), i64::from(attempts), error, at],
        )?;
        RetryOutcome::RetryAt(at)
    };
    tx.commit()?;
    Ok(outcome)
}

/// Re-queues a failed outgoing message (the user pressed "retry").
pub fn message_retry(
    conn: &mut Connection,
    keys: &Keys,
    server: Uuid,
    message: Uuid,
    now: i64,
) -> Result<Option<Message>, StoreError> {
    let tx = conn.transaction()?;
    let n = tx.execute(
        "UPDATE social_outbox SET attempts = 0, next_attempt_at = ?3, last_error = NULL WHERE server_id = ?1 AND message_id = ?2",
        params![text(server), text(message), now],
    )?;
    if n == 0 {
        return Ok(None);
    }
    set_status(&tx, server, message, MessageStatus::Pending)?;
    let out = get_message(&tx, keys, server, message)?;
    tx.commit()?;
    Ok(out)
}

/// Deletes processed-envelope markers older than the server's retention.
pub fn prune_processed(conn: &Connection, server: Uuid, now: i64) -> Result<usize, StoreError> {
    Ok(conn.execute(
        "DELETE FROM social_processed_envelopes WHERE server_id = ?1 AND processed_at < ?2",
        params![text(server), now.saturating_sub(PROCESSED_RETENTION_SECS)],
    )?)
}

// ---------------------------------------------------------------------------------------
// Contacts: verification and safety numbers
// ---------------------------------------------------------------------------------------

fn key_pairs(rows: &[PinnedRow]) -> Vec<(String, String)> {
    rows.iter()
        .filter(|r| r.state != PinState::Revoked)
        .map(|r| (r.device.identity_key.clone(), r.device.signing_key.clone()))
        .collect()
}

fn current_safety_number(
    conn: &Connection,
    keys: &Keys,
    server: Uuid,
    contact: Uuid,
) -> Result<(crypto::SafetyNumber, Vec<PinnedRow>), StoreError> {
    let me = require_account(conn, keys, server)?;
    let mut mine = key_pairs(&pinned_devices(conn, server, me.user_id)?);
    mine.push((me.account.identity_key(), me.account.signing_key()));
    let theirs_rows = pinned_devices(conn, server, contact)?;
    let theirs = key_pairs(&theirs_rows);
    let number = crypto::safety_number((me.user_id, &mine), (contact, &theirs))?;
    Ok((number, theirs_rows))
}

/// Safety number, verification state and devices of `contact`.
pub fn contact_security(
    conn: &Connection,
    keys: &Keys,
    server: Uuid,
    contact: Uuid,
) -> Result<ContactSecurity, StoreError> {
    let (number, devices) = current_safety_number(conn, keys, server, contact)?;
    let digits = number.digits();
    let stored: Option<(Option<String>, i64)> = conn
        .query_row(
            "SELECT verified_safety_number, updated_at FROM social_contacts WHERE server_id = ?1 AND user_id = ?2",
            params![text(server), text(contact)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (verified_number, verified_at) = match stored {
        Some((Some(n), at)) => (Some(n), at),
        _ => (None, i64::MAX),
    };
    let verified = verified_number.as_deref() == Some(digits.as_str());
    let needs_reverification = verified_number.is_some() && !verified;
    Ok(ContactSecurity {
        user_id: contact,
        verified,
        needs_reverification,
        safety_number: digits,
        safety_number_groups: number.groups,
        devices: devices
            .iter()
            .map(|d| ContactDevice {
                device_id: d.device.device_id,
                display_name: d.display_name.clone(),
                first_seen_at: unix_rfc3339(d.first_seen_at),
                key_fingerprint: crypto::key_fingerprint(&d.device.identity_key),
                state: match d.state {
                    PinState::Revoked => ContactDeviceState::Revoked,
                    PinState::KeyChanged => ContactDeviceState::KeyChanged,
                    PinState::Trusted if needs_reverification && d.first_seen_at > verified_at => {
                        ContactDeviceState::New
                    }
                    PinState::Trusted => ContactDeviceState::Trusted,
                },
            })
            .collect(),
    })
}

/// Marks `contact` verified at the current safety number (or clears it).
pub fn set_verified(
    conn: &Connection,
    keys: &Keys,
    server: Uuid,
    contact: Uuid,
    verified: bool,
    now: i64,
) -> Result<ContactSecurity, StoreError> {
    let number = if verified {
        Some(
            current_safety_number(conn, keys, server, contact)?
                .0
                .digits(),
        )
    } else {
        None
    };
    conn.execute(
        "INSERT INTO social_contacts (server_id, user_id, verified_safety_number, updated_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (server_id, user_id) DO UPDATE SET verified_safety_number = excluded.verified_safety_number, updated_at = excluded.updated_at",
        params![text(server), text(contact), number, now],
    )?;
    contact_security(conn, keys, server, contact)
}

// ---------------------------------------------------------------------------------------
// Blocks (kept locally: the server has no list endpoint)
// ---------------------------------------------------------------------------------------

pub fn block_add(
    conn: &Connection,
    server: Uuid,
    user: Uuid,
    username: Option<&str>,
    now: i64,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO social_blocks (server_id, user_id, username, blocked_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (server_id, user_id) DO UPDATE SET username = coalesce(excluded.username, username)",
        params![text(server), text(user), username, now],
    )?;
    Ok(())
}

pub fn block_remove(conn: &Connection, server: Uuid, user: Uuid) -> Result<(), StoreError> {
    conn.execute(
        "DELETE FROM social_blocks WHERE server_id = ?1 AND user_id = ?2",
        params![text(server), text(user)],
    )?;
    Ok(())
}

pub fn blocks_list(
    conn: &Connection,
    server: Uuid,
) -> Result<Vec<(Uuid, Option<String>, i64)>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT user_id, username, blocked_at FROM social_blocks WHERE server_id = ?1 ORDER BY blocked_at DESC",
    )?;
    let rows = stmt
        .query_map([text(server)], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(u, name, at)| Ok((id(&u)?, name, at)))
        .collect()
}

#[cfg(test)]
mod tests;
