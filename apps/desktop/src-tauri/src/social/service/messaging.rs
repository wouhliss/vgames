//! Messaging on the active server (A4-T08, 05-social §4): device registration and
//! one-time-key top-up, conversations, sending through the outbox, receiving from the
//! inbox, device-change notices and safety-number verification.
//!
//! - **Registration.** After every `hello` the launcher makes sure this install's Olm
//!   account exists for the signed-in user and that the session is bound to its device
//!   (`POST /v1/devices`, which re-binds a new session to the same keys). A server that
//!   refuses the keys (`device_keys_in_use`: the device was revoked from another install)
//!   gets a fresh account. Then the server's unclaimed one-time keys are topped up to 50
//!   when fewer than 20 remain, and again after pre-key messages arrive (the only moment
//!   the launcher learns that its keys were used; no polling).
//! - **Sending.** `message_send` stores the message (`pending`) and its payload in the
//!   outbox, both encrypted at rest, and wakes the outbox task. Delivery addresses every
//!   trusted device of the members and this user's other devices, claims keys for devices
//!   without a session (signatures checked against the pinned signing key), encrypts once
//!   per device, posts in batches of 64, and follows `unknown_devices` (fetch the members'
//!   devices, encrypt for the new ones, post again). Failures back off (2 s … 5 min, 12
//!   attempts) on one timer; a message to a device whose key changed is refused before it is
//!   queued (`key_changed`) until the user trusts the new key.
//! - **Receiving.** `inbox.new` (and every `hello`) drains `GET /v1/inbox`: each envelope is
//!   decrypted, de-duplicated and stored in one transaction, then acknowledged. An envelope
//!   from an unknown device fetches its user's devices first; one that can never be
//!   decrypted is acknowledged and dropped (it would block the inbox forever).
//! - **Devices.** `device.added` / `device.revoked` update the pins and store a notice in each
//!   conversation with that user; a changed key is set aside and blocks sending to it.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde_json::Value;
use time::OffsetDateTime;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_proto::realtime::{DeviceEvent, Hello, InboxNew, Typing, TypingStart, kinds};
use vgames_proto::social::{
    ClaimKeysRequest, ClaimedKey, ConversationCreate, DeviceRegister, InboxAck, InboxEnvelope,
    MAX_CLAIM_DEVICES, MAX_ENVELOPES_PER_SEND, MAX_PARTY_MEMBERS, OsFamily, SendMessageRequest,
};

use super::{SocialService, lock};
use crate::db::now_unix;
use crate::social::api::SocialApi;
use crate::social::crypto::{self, MESSAGE_TYPE_PRE_KEY};
use crate::social::model::{
    ContactSecurity, Conversation, ConversationKind, DevicePlatform, Message, MessageStatus,
    MyDevice, SocialError, SocialLimit, UserSummary, rfc3339,
};
use crate::social::payload::{Payload, PayloadError};
use crate::social::store::{
    self, CachedConversation, DeviceChanges, Keys, ReceiveError, Received, RetryOutcome, StoreError,
};

/// How many due outbox entries one pass reads.
const OUTBOX_BATCH: u32 = 16;
/// Rounds of "send, then follow `unknown_devices`" per attempt.
const DELIVERY_ROUNDS: usize = 3;
/// Inbox pages per drain (100 envelopes each); the next `inbox.new` continues.
const INBOX_PAGES: usize = 50;
/// Conversation pages per sync (100 each).
const CONVERSATION_PAGES: usize = 20;
/// One `typing` frame per conversation per this interval.
const TYPING_EVERY: Duration = Duration::from_secs(3);

/// The device this install registered for the current session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ready {
    server: Uuid,
    user: Uuid,
    device: Uuid,
}

/// Messaging state kept by [`SocialService`].
#[derive(Default)]
pub(super) struct MessagingState {
    ready: Mutex<Option<Ready>>,
    /// One inbox drain at a time.
    drain: tokio::sync::Mutex<()>,
    /// Wakes the outbox task.
    outbox: Notify,
    /// Users whose device list was fetched during this run, per server.
    synced: Mutex<HashSet<(Uuid, Uuid)>>,
    typing_sent: Mutex<HashMap<Uuid, Instant>>,
}

/// This platform, as the server names it.
pub fn platform() -> OsFamily {
    if cfg!(target_os = "windows") {
        OsFamily::Windows
    } else if cfg!(target_os = "macos") {
        OsFamily::Macos
    } else {
        OsFamily::Linux
    }
}

/// The default device name: the computer's name, else the platform.
pub fn default_device_name() -> String {
    let name: String = sysinfo::System::host_name()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    let name = name.trim();
    if name.is_empty() {
        match platform() {
            OsFamily::Windows => "Windows PC",
            OsFamily::Macos => "Mac",
            OsFamily::Linux => "Linux PC",
        }
        .to_owned()
    } else {
        name.to_owned()
    }
}

fn user_summary(u: vgames_proto::auth::UserPublic) -> UserSummary {
    u.into()
}

fn cached(c: vgames_proto::social::Conversation) -> CachedConversation {
    let created_at = c.created_at.unix_timestamp();
    CachedConversation {
        id: c.id,
        kind: match c.kind {
            vgames_proto::social::ConversationKind::Direct => ConversationKind::Direct,
            vgames_proto::social::ConversationKind::Party => ConversationKind::Party,
        },
        members: c.members.into_iter().map(user_summary).collect(),
        created_at,
        last_activity_at: c
            .last_activity_at
            .map_or(created_at, OffsetDateTime::unix_timestamp),
    }
}

/// Index of the envelope a `400 unknown_recipient` names (`envelopes[i].recipient_device_id`).
fn refused_envelope(error: &SocialError) -> Option<usize> {
    let SocialError::InvalidInput { field, .. } = error else {
        return None;
    };
    field
        .strip_prefix("envelopes[")?
        .strip_suffix("].recipient_device_id")?
        .parse()
        .ok()
}

/// Some recipients had no key to claim: retried with backoff like a network failure.
fn keys_unavailable(devices: usize) -> SocialError {
    SocialError::Server {
        code: KEYS_UNAVAILABLE.to_owned(),
        message: format!("{devices} recipient device(s) have no keys yet"),
    }
}

const KEYS_UNAVAILABLE: &str = "keys_unavailable";

fn is_device_required(error: &SocialError) -> bool {
    matches!(error, SocialError::Server { code, .. } if code == "device_required")
}

/// Whether a failed send will never succeed as is (the message becomes `failed`).
fn is_permanent(error: &SocialError) -> bool {
    match error {
        SocialError::Offline
        | SocialError::RateLimited { .. }
        | SocialError::NotSignedIn
        | SocialError::Internal { .. } => false,
        SocialError::Server { code, .. } => code != "device_required" && code != KEYS_UNAVAILABLE,
        _ => true,
    }
}

fn store_error(context: &'static str) -> impl Fn(StoreError) -> SocialError {
    move |e| SocialError::internal(context, &e)
}

impl SocialService {
    fn keys(&self) -> std::sync::Arc<Keys> {
        self.inner.keys.clone()
    }

    /// Runs `f` on the database thread with the social keys.
    async fn with_store<T, F>(&self, context: &'static str, f: F) -> Result<T, SocialError>
    where
        F: FnOnce(&mut Connection, &Keys) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        let keys = self.keys();
        self.inner
            .db
            .call(move |c| Ok(f(c, &keys)))
            .await
            .map_err(|e| SocialError::internal(context, &e))?
            .map_err(store_error(context))
    }

    fn server_id(&self) -> Result<Uuid, SocialError> {
        Ok(self
            .inner
            .sessions
            .current()
            .ok_or(SocialError::NotSignedIn)?
            .server_id)
    }

    fn ready_device(&self, api: &SocialApi<'_>) -> Option<Uuid> {
        let ready = *lock(&self.inner.messaging.ready);
        ready
            .filter(|r| r.server == api.session.server_id && r.user == api.session.user_id)
            .map(|r| r.device)
    }

    // ---- registration and keys ------------------------------------------------------------

    /// Makes sure the session is bound to this install's device (after every `hello`, or
    /// when the server says the session has no device).
    async fn ensure_device(
        &self,
        api: &SocialApi<'_>,
        bound: Option<Uuid>,
    ) -> Result<Uuid, SocialError> {
        let server = api.session.server_id;
        let user = api.session.user_id;
        let now = now_unix();
        let mut account = self
            .with_store("creating the chat account", move |c, k| {
                store::ensure_account(c, k, server, user, now)
            })
            .await?;
        let mut replaced = false;
        let device = loop {
            if let Some(d) = account.device_id
                && bound == Some(d)
            {
                break d;
            }
            let keys = self
                .with_store("reading the device keys", move |c, k| {
                    store::signed_device_keys(c, k, server)
                })
                .await?;
            let body = DeviceRegister {
                display_name: self.inner.device_name.clone(),
                platform: platform(),
                identity_key: keys.identity_key,
                signing_key: keys.signing_key,
                keys_signature: keys.keys_signature,
            };
            match api.register_device(&body).await {
                Ok(device) => {
                    let id = device.id;
                    self.with_store("saving the device id", move |c, _| {
                        store::set_device_id(c, server, id, now_unix())
                    })
                    .await?;
                    tracing::info!(%server, device_id = %id, "chat device registered");
                    break id;
                }
                Err(SocialError::Conflict { code, .. })
                    if code == "device_keys_in_use" && !replaced =>
                {
                    tracing::warn!(%server, "the server refused this install's chat keys; creating new ones");
                    replaced = true;
                    account = self
                        .with_store("replacing the chat account", move |c, k| {
                            store::replace_account(c, k, server, now_unix())
                        })
                        .await?;
                }
                Err(e) => return Err(e),
            }
        };
        *lock(&self.inner.messaging.ready) = Some(Ready {
            server,
            user,
            device,
        });
        Ok(device)
    }

    /// Uploads one-time keys when the server holds fewer than 20, and a fallback key when
    /// it has none.
    async fn top_up_keys(&self, api: &SocialApi<'_>, device: Uuid) -> Result<(), SocialError> {
        let server = api.session.server_id;
        let mine = api
            .my_devices()
            .await?
            .items
            .into_iter()
            .find(|d| d.id == device)
            .ok_or(SocialError::NotFound)?;
        let available = usize::try_from(mine.one_time_keys_available.unwrap_or(0)).unwrap_or(0);
        let has_fallback = mine.has_fallback_key.unwrap_or(false);
        if available >= crypto::ONE_TIME_KEY_LOW_WATER && has_fallback {
            return Ok(());
        }
        let upload = self
            .with_store("preparing one-time keys", move |c, k| {
                store::prepare_top_up(c, k, server, available, !has_fallback, now_unix())
            })
            .await?;
        if upload.one_time_keys.is_empty() && upload.fallback_key.is_none() {
            return Ok(());
        }
        let stored = api.upload_keys(device, &upload).await?;
        self.with_store("publishing one-time keys", move |c, k| {
            store::mark_keys_published(c, k, server, now_unix())
        })
        .await?;
        tracing::debug!(%server, available = stored.available, "one-time keys topped up");
        Ok(())
    }

    /// `hello` on a new connection: register, top up keys, resync conversations and the
    /// inbox, then retry the outbox.
    pub(super) async fn messaging_connected(&self, hello: &Hello) {
        let Ok(api) = self.api() else { return };
        let device = match self.ensure_device(&api, hello.device_id).await {
            Ok(d) => d,
            Err(error) => {
                tracing::warn!(%error, "chat device registration failed");
                return;
            }
        };
        if let Err(error) = self.top_up_keys(&api, device).await {
            tracing::debug!(%error, "one-time key top-up failed");
        }
        if let Err(error) = self.sync_conversations(&api).await {
            tracing::debug!(%error, "conversation resync failed");
        }
        self.drain_inbox(&api).await;
        self.inner.messaging.outbox.notify_one();
    }

    // ---- realtime events ------------------------------------------------------------------

    pub(super) async fn messaging_event(&self, server_id: Uuid, kind: &str, data: Value) {
        let Ok(api) = self.api() else { return };
        if api.session.server_id != server_id {
            return;
        }
        match kind {
            kinds::INBOX_NEW => {
                if serde_json::from_value::<InboxNew>(data).is_ok() {
                    self.drain_inbox(&api).await;
                }
            }
            kinds::DEVICE_ADDED => {
                let Ok(e) = serde_json::from_value::<DeviceEvent>(data) else {
                    return;
                };
                if let Err(error) = self.sync_devices(&api, e.user_id).await {
                    tracing::debug!(%error, "device list refresh failed");
                }
                // A new device of a member: messages still queued should reach it too.
                self.inner.messaging.outbox.notify_one();
            }
            kinds::DEVICE_REVOKED => {
                let Ok(e) = serde_json::from_value::<DeviceEvent>(data) else {
                    return;
                };
                let now = now_unix();
                let revoked = self
                    .with_store("revoking a device", move |c, _| {
                        store::mark_device_revoked(c, server_id, e.device_id, now)
                    })
                    .await;
                match revoked {
                    Ok(Some(user)) if user == e.user_id => {
                        let changes = DeviceChanges {
                            revoked: vec![e.device_id],
                            ..DeviceChanges::default()
                        };
                        self.announce(server_id, &api, user, &changes).await;
                    }
                    Ok(_) => {}
                    Err(error) => tracing::warn!(%error, "device revocation not applied"),
                }
            }
            kinds::TYPING => {
                let Ok(t) = serde_json::from_value::<Typing>(data) else {
                    return;
                };
                if t.user_id != api.session.user_id {
                    self.inner.events.typing(t.conversation_id, t.user_id);
                }
            }
            _ => {}
        }
    }

    // ---- devices ----------------------------------------------------------------------------

    /// Fetches `user`'s devices, updates the pins and announces changes.
    async fn sync_devices(
        &self,
        api: &SocialApi<'_>,
        user: Uuid,
    ) -> Result<DeviceChanges, SocialError> {
        let server = api.session.server_id;
        let list = api.user_devices(user).await?;
        let now = now_unix();
        let changes = self
            .with_store("updating device pins", move |c, k| {
                store::sync_user_devices(c, k, server, user, &list.items, now)
            })
            .await?;
        lock(&self.inner.messaging.synced).insert((server, user));
        if !changes.rejected.is_empty() {
            tracing::warn!(%user, rejected = changes.rejected.len(), "devices with bad signatures ignored");
        }
        self.announce(server, api, user, &changes).await;
        Ok(changes)
    }

    /// Stores and emits notices for device changes of `user` (nothing on first sight).
    async fn announce(
        &self,
        server: Uuid,
        api: &SocialApi<'_>,
        user: Uuid,
        changes: &DeviceChanges,
    ) {
        let notices = changes.notices(user);
        if notices.is_empty() {
            return;
        }
        let own = user == api.session.user_id;
        let conversations = if own {
            Vec::new()
        } else {
            self.with_store("reading conversations", move |c, _| {
                store::conversations_with(c, server, user)
            })
            .await
            .unwrap_or_default()
        };
        for notice in notices {
            if conversations.is_empty() {
                self.inner.events.device_notice(None, &notice);
                continue;
            }
            for conversation in &conversations {
                let conversation = *conversation;
                let n = notice.clone();
                let now = now_unix();
                let stored = self
                    .with_store("storing a device notice", move |c, k| {
                        store::insert_notice(c, k, server, conversation, n, now)
                    })
                    .await;
                if let Ok(Some(message)) = stored {
                    self.inner.events.message_received(&message);
                }
                self.inner.events.device_notice(Some(conversation), &notice);
            }
        }
        if !conversations.is_empty() {
            self.emit_conversations(server).await;
        }
    }

    /// Own devices on the active server.
    pub async fn devices_list(&self) -> Result<Vec<MyDevice>, SocialError> {
        let api = self.api()?;
        Ok(api
            .my_devices()
            .await?
            .items
            .into_iter()
            .map(|d| MyDevice {
                id: d.id,
                display_name: d.display_name,
                platform: match d.platform {
                    OsFamily::Windows => DevicePlatform::Windows,
                    OsFamily::Linux => DevicePlatform::Linux,
                    OsFamily::Macos => DevicePlatform::Macos,
                },
                current: d.current.unwrap_or(false),
                created_at: rfc3339(d.created_at),
                last_seen_at: d.last_seen_at.map(rfc3339),
            })
            .collect())
    }

    /// Revokes one of own devices. It stops receiving at once; its sessions end.
    pub async fn device_revoke(&self, device_id: Uuid) -> Result<(), SocialError> {
        let api = self.api()?;
        let server = api.session.server_id;
        api.revoke_device(device_id).await?;
        let now = now_unix();
        self.with_store("revoking a device", move |c, _| {
            store::mark_device_revoked(c, server, device_id, now)
        })
        .await?;
        Ok(())
    }

    // ---- verification -----------------------------------------------------------------------

    /// Refreshes `user`'s devices when online (best effort) and returns the safety number.
    pub async fn contact_security(&self, user_id: Uuid) -> Result<ContactSecurity, SocialError> {
        let api = self.api()?;
        let server = api.session.server_id;
        match self.sync_devices(&api, user_id).await {
            Ok(_) | Err(SocialError::Offline) => {}
            Err(e) => return Err(e),
        }
        self.with_store("computing the safety number", move |c, k| {
            store::contact_security(c, k, server, user_id)
        })
        .await
    }

    pub async fn contact_set_verified(
        &self,
        user_id: Uuid,
        verified: bool,
    ) -> Result<ContactSecurity, SocialError> {
        let server = self.server_id()?;
        let now = now_unix();
        self.with_store("saving verification", move |c, k| {
            store::set_verified(c, k, server, user_id, verified, now)
        })
        .await
    }

    /// Accepts the changed key of a contact's device; sending to it works again.
    pub async fn contact_trust_device(
        &self,
        user_id: Uuid,
        device_id: Uuid,
    ) -> Result<ContactSecurity, SocialError> {
        let server = self.server_id()?;
        let now = now_unix();
        let trusted = self
            .with_store("trusting a device", move |c, _| {
                store::trust_device(c, server, user_id, device_id, now)
            })
            .await?;
        if !trusted {
            return Err(SocialError::NotFound);
        }
        self.with_store("computing the safety number", move |c, k| {
            store::contact_security(c, k, server, user_id)
        })
        .await
    }

    // ---- conversations ----------------------------------------------------------------------

    /// Fetches the full conversation list and replaces the cache.
    async fn sync_conversations(&self, api: &SocialApi<'_>) -> Result<(), SocialError> {
        let server = api.session.server_id;
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..CONVERSATION_PAGES {
            let page = api.conversations(cursor.as_deref()).await?;
            all.extend(page.items.into_iter().map(cached));
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        self.with_store("caching conversations", move |c, _| {
            store::replace_conversations(c, server, &all)
        })
        .await?;
        self.emit_conversations(server).await;
        Ok(())
    }

    async fn cached_list(&self, server: Uuid) -> Result<Vec<Conversation>, SocialError> {
        self.with_store("reading conversations", move |c, k| {
            store::list_conversations(c, k, server)
        })
        .await
    }

    async fn emit_conversations(&self, server: Uuid) {
        match self.cached_list(server).await {
            Ok(list) => self.inner.events.conversations_changed(&list),
            Err(error) => tracing::debug!(%error, "conversation list not emitted"),
        }
    }

    /// Conversations, most recent first. Offline, the cached list.
    pub async fn conversations_list(&self) -> Result<Vec<Conversation>, SocialError> {
        let api = self.api()?;
        let server = api.session.server_id;
        match self.sync_conversations(&api).await {
            Ok(()) | Err(SocialError::Offline) => {}
            Err(e) => return Err(e),
        }
        self.cached_list(server).await
    }

    async fn open(
        &self,
        api: &SocialApi<'_>,
        body: ConversationCreate,
    ) -> Result<Conversation, SocialError> {
        let server = api.session.server_id;
        let conversation = cached(api.create_conversation(&body).await?);
        let id = conversation.id;
        let out = self
            .with_store("caching a conversation", move |c, k| {
                store::upsert_conversation(c, server, &conversation)?;
                store::get_conversation(c, k, server, id)
            })
            .await?
            .ok_or(SocialError::NotFound)?;
        self.emit_conversations(server).await;
        Ok(out)
    }

    /// The direct conversation with a friend (created on first use).
    pub async fn conversation_open_direct(
        &self,
        user_id: Uuid,
    ) -> Result<Conversation, SocialError> {
        let api = self.api()?;
        match self
            .open(&api, ConversationCreate::Direct { user_id })
            .await
        {
            // "not_allowed" (not friends) looks like an unknown user, as everywhere else.
            Err(SocialError::Server { code, .. }) if code == "not_allowed" => {
                Err(SocialError::NotFound)
            }
            other => other,
        }
    }

    /// A party with 1–15 friends.
    pub async fn conversation_create_party(
        &self,
        user_ids: Vec<Uuid>,
    ) -> Result<Conversation, SocialError> {
        let mut unique = user_ids.clone();
        unique.sort_unstable();
        unique.dedup();
        if unique.is_empty() {
            return Err(SocialError::invalid("user_ids", "Pick at least one friend"));
        }
        if unique.len() >= MAX_PARTY_MEMBERS {
            return Err(SocialError::LimitReached {
                limit: SocialLimit::PartyMembers,
            });
        }
        let api = self.api()?;
        match self
            .open(&api, ConversationCreate::Party { user_ids: unique })
            .await
        {
            Err(SocialError::Server { code, .. }) if code == "not_allowed" => {
                Err(SocialError::NotFound)
            }
            other => other,
        }
    }

    /// Messages of a conversation, oldest first, `limit` (1–200) before `before`.
    pub async fn messages_list(
        &self,
        conversation_id: Uuid,
        before: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<Message>, SocialError> {
        if !(1..=200).contains(&limit) {
            return Err(SocialError::invalid("limit", "1 to 200"));
        }
        let server = self.server_id()?;
        self.with_store("reading messages", move |c, k| {
            store::list_messages(c, k, server, conversation_id, before, limit)
        })
        .await
    }

    pub async fn conversation_mark_read(&self, conversation_id: Uuid) -> Result<(), SocialError> {
        let server = self.server_id()?;
        let found = self
            .with_store("marking read", move |c, _| {
                store::mark_read(c, server, conversation_id)
            })
            .await?;
        if !found {
            return Err(SocialError::NotFound);
        }
        self.emit_conversations(server).await;
        Ok(())
    }

    /// Sends `typing` for a conversation, at most once every 3 s.
    pub fn typing_start(&self, conversation_id: Uuid) -> Result<(), SocialError> {
        self.server_id()?;
        let now = Instant::now();
        {
            let mut sent = lock(&self.inner.messaging.typing_sent);
            sent.retain(|_, at| now.duration_since(*at) < TYPING_EVERY);
            if sent.contains_key(&conversation_id) {
                return Ok(());
            }
            sent.insert(conversation_id, now);
        }
        if let Ok(data) = serde_json::to_value(TypingStart { conversation_id }) {
            self.inner.realtime.send(kinds::TYPING, data);
        }
        Ok(())
    }

    /// Member user ids of a conversation (this user included), from the cache or the server.
    async fn members(
        &self,
        api: &SocialApi<'_>,
        conversation: Uuid,
    ) -> Result<Vec<Uuid>, SocialError> {
        let server = api.session.server_id;
        for attempt in 0..2 {
            let members = self
                .with_store("reading members", move |c, _| {
                    store::conversation_members(c, server, conversation)
                })
                .await?;
            if let Some((_, members)) = members {
                let mut ids: Vec<Uuid> = members.into_iter().map(|u| u.id).collect();
                if !ids.contains(&api.session.user_id) {
                    ids.push(api.session.user_id);
                }
                return Ok(ids);
            }
            if attempt == 0 {
                self.sync_conversations(api).await?;
            }
        }
        Err(SocialError::NotFound)
    }

    /// Fetches the devices of members never seen during this run.
    async fn sync_unseen(&self, api: &SocialApi<'_>, users: &[Uuid]) -> Result<(), SocialError> {
        let server = api.session.server_id;
        for user in users {
            if lock(&self.inner.messaging.synced).contains(&(server, *user)) {
                continue;
            }
            match self.sync_devices(api, *user).await {
                // A member who blocked us, or left: nothing to address.
                Ok(_) | Err(SocialError::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    // ---- sending ----------------------------------------------------------------------------

    /// Stores a text message and queues it; `message-status-changed` follows.
    pub async fn message_send(
        &self,
        conversation_id: Uuid,
        text: String,
    ) -> Result<Message, SocialError> {
        let api = self.api()?;
        let server = api.session.server_id;
        let payload = Payload::text(
            conversation_id,
            Uuid::now_v7(),
            OffsetDateTime::now_utc(),
            &text,
        )
        .map_err(|e| match e {
            PayloadError::Empty => SocialError::invalid("text", "Write a message first"),
            _ => SocialError::invalid("text", "Messages are at most 4,000 characters"),
        })?;
        let members = self.members(&api, conversation_id).await?;
        match self.sync_unseen(&api, &members).await {
            Ok(()) | Err(SocialError::Offline) => {}
            Err(e) => return Err(e),
        }
        let body = crate::social::model::MessageBody::Text {
            text: match &payload.content {
                crate::social::payload::Content::Text { body } => body.clone(),
                _ => String::new(),
            },
        };
        let now = now_unix();
        let registered = self
            .with_store("reading the chat account", move |c, k| {
                store::account_info(c, k, server)
            })
            .await?
            .is_some_and(|a| a.device_id.is_some());
        if !registered {
            // Never connected to this server yet: nothing can be encrypted.
            return Err(SocialError::Offline);
        }
        let message = self
            .with_store("queueing a message", move |c, k| {
                let targets = store::targets(c, k, server, &members)?;
                if let Some((user, _)) = targets.key_changed.first() {
                    let user = *user;
                    let device_ids = targets
                        .key_changed
                        .iter()
                        .filter(|(u, _)| *u == user)
                        .map(|(_, d)| *d)
                        .collect();
                    return Ok(Err(SocialError::KeyChanged {
                        user_id: user,
                        device_ids,
                    }));
                }
                store::create_outgoing(c, k, server, &payload, body, now).map(Ok)
            })
            .await??;
        self.emit_conversations(server).await;
        self.inner.messaging.outbox.notify_one();
        Ok(message)
    }

    /// Queues a failed message again.
    pub async fn message_retry(&self, message_id: Uuid) -> Result<Message, SocialError> {
        let server = self.server_id()?;
        let now = now_unix();
        let message = self
            .with_store("retrying a message", move |c, k| {
                store::message_retry(c, k, server, message_id, now)
            })
            .await?
            .ok_or(SocialError::NotFound)?;
        self.inner.events.message_status_changed(
            message.id,
            message.conversation_id,
            MessageStatus::Pending,
        );
        self.inner.messaging.outbox.notify_one();
        Ok(message)
    }

    /// Wakes the outbox when a message is due; no polling while it is empty.
    pub(super) async fn outbox_loop(self, shutdown: CancellationToken) {
        let mut sessions = self.inner.sessions.subscribe();
        loop {
            // Timers only run while a registered device can send (no busy loop before that).
            let server = self
                .api()
                .ok()
                .filter(|api| self.ready_device(api).is_some())
                .map(|api| api.session.server_id);
            let next = match server {
                Some(server) => self
                    .inner
                    .db
                    .call(move |c| Ok(store::outbox_next_due(c, server)))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .flatten(),
                None => None,
            };
            let wait = next.map(|at| {
                Duration::from_secs(u64::try_from(at.saturating_sub(now_unix())).unwrap_or(0))
            });
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = self.inner.messaging.outbox.notified() => {}
                r = sessions.changed() => if r.is_err() { return },
                () = async {
                    match wait {
                        Some(d) => tokio::time::sleep(d).await,
                        None => std::future::pending().await,
                    }
                } => {}
            }
            if let Ok(api) = self.api()
                && self.ready_device(&api).is_some()
            {
                self.flush_outbox(&api).await;
            }
        }
    }

    /// Sends every due outbox entry, oldest first. Stops at the first transient failure.
    async fn flush_outbox(&self, api: &SocialApi<'_>) {
        let server = api.session.server_id;
        loop {
            let now = now_unix();
            let due = match self
                .with_store("reading the outbox", move |c, k| {
                    store::outbox_due(c, k, server, now, OUTBOX_BATCH)
                })
                .await
            {
                Ok(d) => d,
                Err(error) => {
                    tracing::warn!(%error, "outbox unreadable");
                    return;
                }
            };
            if due.is_empty() {
                return;
            }
            for item in due {
                let cmid = item.client_message_id;
                match self.deliver(api, &item).await {
                    Ok(()) => {
                        let sent = self
                            .with_store("marking a message sent", move |c, _| {
                                store::outbox_sent(c, server, cmid)
                            })
                            .await;
                        if let Ok(Some((message, conversation))) = sent {
                            self.inner.events.message_status_changed(
                                message,
                                conversation,
                                MessageStatus::Sent,
                            );
                        }
                    }
                    Err(error) => {
                        let permanent = is_permanent(&error);
                        tracing::info!(%error, permanent, attempts = item.attempts, "message not sent");
                        let reason = error.to_string();
                        let now = now_unix();
                        let outcome = self
                            .with_store("recording a send failure", move |c, _| {
                                store::outbox_attempt_failed(
                                    c, server, cmid, now, &reason, permanent,
                                )
                            })
                            .await;
                        if let Ok(RetryOutcome::Failed {
                            message_id,
                            conversation_id,
                        }) = outcome
                        {
                            self.inner.events.message_status_changed(
                                message_id,
                                conversation_id,
                                MessageStatus::Failed,
                            );
                        }
                        if !permanent {
                            return;
                        }
                    }
                }
            }
        }
    }

    /// Claims one key per device (64 per call). Devices with no key left are absent.
    async fn claim(
        &self,
        api: &SocialApi<'_>,
        devices: &[Uuid],
    ) -> Result<Vec<ClaimedKey>, SocialError> {
        let mut out = Vec::new();
        for chunk in devices.chunks(MAX_CLAIM_DEVICES) {
            let list = api
                .claim_keys(&ClaimKeysRequest {
                    device_ids: chunk.to_vec(),
                })
                .await?;
            out.extend(
                list.items
                    .into_iter()
                    .filter(|k| chunk.contains(&k.device_id)),
            );
        }
        Ok(out)
    }

    /// One delivery attempt of an outbox entry (05-social §4.1 steps 2–5).
    async fn deliver(
        &self,
        api: &SocialApi<'_>,
        item: &store::OutboxItem,
    ) -> Result<(), SocialError> {
        let server = api.session.server_id;
        let members = self.members(api, item.conversation_id).await?;
        self.sync_unseen(api, &members).await?;
        let mut served: HashSet<Uuid> = HashSet::new();
        let mut excluded: HashSet<Uuid> = HashSet::new();
        let mut keyless: HashSet<Uuid> = HashSet::new();
        let mut device_required = false;
        for _ in 0..DELIVERY_ROUNDS {
            let users = members.clone();
            let targets = self
                .with_store("reading devices", move |c, k| {
                    store::targets(c, k, server, &users)
                })
                .await?;
            if let Some((user, _)) = targets.key_changed.first() {
                return Err(SocialError::KeyChanged {
                    user_id: *user,
                    device_ids: targets
                        .key_changed
                        .iter()
                        .filter(|(u, _)| u == user)
                        .map(|(_, d)| *d)
                        .collect(),
                });
            }
            let devices: Vec<Uuid> = targets
                .trusted
                .iter()
                .map(|(_, d)| *d)
                .filter(|d| !served.contains(d) && !excluded.contains(d))
                .collect();
            if devices.is_empty() {
                return if keyless.is_empty() {
                    Ok(())
                } else {
                    Err(keys_unavailable(keyless.len()))
                };
            }
            let list = devices.clone();
            let need = self
                .with_store("reading sessions", move |c, _| {
                    store::devices_without_session(c, server, &list)
                })
                .await?;
            let claimed = if need.is_empty() {
                Vec::new()
            } else {
                self.claim(api, &need).await?
            };
            let plaintext = item.plaintext.clone();
            let now = now_unix();
            let encrypted = self
                .with_store("encrypting a message", move |c, k| {
                    store::encrypt_for(c, k, server, &devices, &claimed, &plaintext, now)
                })
                .await?;
            // No key could be claimed (the device never uploaded any): deliver to the others
            // now and retry this device later, so the message is never "sent" to nobody.
            keyless.extend(encrypted.missing.iter().copied());
            excluded.extend(encrypted.missing.iter().copied());
            excluded.extend(encrypted.unknown.iter().copied());
            let mut unknown: Vec<Uuid> = Vec::new();
            let mut refused = false;
            for chunk in encrypted.envelopes.chunks(MAX_ENVELOPES_PER_SEND) {
                let body = SendMessageRequest {
                    client_message_id: item.client_message_id,
                    envelopes: chunk.to_vec(),
                };
                match api.send_message(item.conversation_id, &body).await {
                    Ok(resp) => {
                        served.extend(chunk.iter().map(|e| e.recipient_device_id));
                        unknown.extend(resp.unknown_devices);
                    }
                    Err(e) => {
                        if let Some(i) = refused_envelope(&e)
                            && let Some(env) = chunk.get(i)
                        {
                            // Revoked or no longer addressable: refresh and leave it out.
                            excluded.insert(env.recipient_device_id);
                            refused = true;
                            continue;
                        }
                        if is_device_required(&e) && !device_required {
                            device_required = true;
                            self.ensure_device(api, None).await?;
                            refused = true;
                            continue;
                        }
                        return Err(e);
                    }
                }
            }
            unknown.retain(|d| !served.contains(d) && !excluded.contains(d));
            if unknown.is_empty() && !refused {
                return if keyless.is_empty() {
                    Ok(())
                } else {
                    Err(keys_unavailable(keyless.len()))
                };
            }
            // New or removed member devices: refresh every member's list, then go again.
            for user in &members {
                match self.sync_devices(api, *user).await {
                    Ok(_) | Err(SocialError::NotFound) => {}
                    Err(e) => return Err(e),
                }
            }
        }
        if keyless.is_empty() {
            Ok(())
        } else {
            Err(keys_unavailable(keyless.len()))
        }
    }

    // ---- receiving --------------------------------------------------------------------------

    /// Fetches, decrypts, stores and acknowledges every waiting envelope.
    async fn drain_inbox(&self, api: &SocialApi<'_>) {
        let _one = self.inner.messaging.drain.lock().await;
        let server = api.session.server_id;
        let mut stored = false;
        let mut pre_keys = false;
        let mut unknown_conversation = false;
        let mut retried_device = false;
        for _ in 0..INBOX_PAGES {
            let page = match api.inbox().await {
                Ok(p) => p,
                Err(e) if is_device_required(&e) && !retried_device => {
                    retried_device = true;
                    if let Err(error) = self.ensure_device(api, None).await {
                        tracing::warn!(%error, "chat device registration failed");
                        return;
                    }
                    continue;
                }
                Err(error) => {
                    tracing::debug!(%error, "inbox fetch failed");
                    break;
                }
            };
            if page.items.is_empty() {
                break;
            }
            let mut ack = Vec::with_capacity(page.items.len());
            let mut blocked = false;
            for env in &page.items {
                match self.receive_one(api, env).await {
                    Some(received) => {
                        ack.push(env.id);
                        pre_keys |= env.olm_message_type == MESSAGE_TYPE_PRE_KEY;
                        if let Some(message) = received {
                            stored = true;
                            let conversation = message.conversation_id;
                            if !self
                                .with_store("reading conversations", move |c, _| {
                                    store::is_cached(c, server, conversation)
                                })
                                .await
                                .unwrap_or(true)
                            {
                                unknown_conversation = true;
                            }
                            self.inner.events.message_received(&message);
                        }
                    }
                    None => {
                        blocked = true;
                        break;
                    }
                }
            }
            if !ack.is_empty()
                && let Err(error) = api.ack(&InboxAck { ids: ack }).await
            {
                tracing::debug!(%error, "inbox ack failed");
                break;
            }
            if blocked || page.next_cursor.is_none() {
                break;
            }
        }
        if unknown_conversation {
            if let Err(error) = self.sync_conversations(api).await {
                tracing::debug!(%error, "conversation resync failed");
                self.emit_conversations(server).await;
            }
        } else if stored {
            self.emit_conversations(server).await;
        }
        if pre_keys && let Some(device) = self.ready_device(api) {
            if let Err(error) = self.top_up_keys(api, device).await {
                tracing::debug!(%error, "one-time key top-up failed");
            }
            let now = now_unix();
            let _ = self
                .with_store("pruning", move |c, _| {
                    store::prune_processed(c, server, now)
                })
                .await;
        }
    }

    /// Processes one envelope. `Some(message)` = acknowledge (with the stored message to
    /// show, if any); `None` = a local failure: keep it on the server and stop draining.
    async fn receive_one(
        &self,
        api: &SocialApi<'_>,
        env: &InboxEnvelope,
    ) -> Option<Option<Message>> {
        let server = api.session.server_id;
        let me = api.session.user_id;
        let mut refreshed = false;
        loop {
            let e = env.clone();
            let now = now_unix();
            let keys = self.keys();
            let result = self
                .inner
                .db
                .call(move |c| Ok(store::receive(c, &keys, server, me, &e, now)))
                .await;
            let result = match result {
                Ok(r) => r,
                Err(error) => {
                    tracing::warn!(%error, "database unavailable while receiving");
                    return None;
                }
            };
            match result {
                Ok(Received::Message(m)) => return Some(Some(m)),
                Ok(Received::InviteJoin { message, .. }) => {
                    // Acted on by the invites client (A4-T09); the secret is never stored.
                    return Some(Some(message));
                }
                Ok(Received::Receipt { .. } | Received::Duplicate) => return Some(None),
                Err(
                    ReceiveError::UnknownSender { user_id, .. }
                    | ReceiveError::KeyMismatch { user_id, .. },
                ) if !refreshed => {
                    refreshed = true;
                    match self.sync_devices(api, user_id).await {
                        Ok(_) | Err(SocialError::NotFound) => continue,
                        Err(SocialError::Offline) => return None,
                        Err(error) => {
                            tracing::debug!(%error, "sender devices unavailable");
                            continue;
                        }
                    }
                }
                Err(ReceiveError::Store(StoreError::Db(error))) => {
                    tracing::warn!(%error, "message not stored; will retry");
                    return None;
                }
                Err(error) => {
                    tracing::warn!(envelope = %env.id, sender_device = %env.sender_device_id, %error, "dropping an envelope");
                    let id = env.id;
                    let now = now_unix();
                    let _ = self
                        .with_store("dropping an envelope", move |c, _| {
                            store::mark_envelope_processed(c, server, id, now)
                        })
                        .await;
                    return Some(None);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_envelopes_are_found_by_index() {
        let e = SocialError::invalid("envelopes[12].recipient_device_id", "x");
        assert_eq!(refused_envelope(&e), Some(12));
        for field in [
            "envelopes[12].ciphertext",
            "envelopes[x].recipient_device_id",
            "recipient_device_id",
        ] {
            assert_eq!(refused_envelope(&SocialError::invalid(field, "x")), None);
        }
        assert_eq!(refused_envelope(&SocialError::NotFound), None);
    }

    #[test]
    fn transient_failures_are_retried() {
        for e in [
            SocialError::Offline,
            SocialError::RateLimited {
                retry_after_seconds: 3,
            },
            SocialError::NotSignedIn,
            SocialError::Internal {
                detail: "db".into(),
            },
            SocialError::Server {
                code: "device_required".into(),
                message: String::new(),
            },
            keys_unavailable(2),
        ] {
            assert!(!is_permanent(&e), "{e:?}");
        }
        for e in [
            SocialError::NotFound,
            SocialError::KeyChanged {
                user_id: Uuid::nil(),
                device_ids: vec![],
            },
            SocialError::Server {
                code: "payload_too_large".into(),
                message: String::new(),
            },
            SocialError::invalid("envelopes", "x"),
        ] {
            assert!(is_permanent(&e), "{e:?}");
        }
    }

    #[test]
    fn device_names_are_printable_and_bounded() {
        let name = default_device_name();
        assert!(!name.is_empty() && name.chars().count() <= 64);
        assert!(!name.chars().any(char::is_control));
    }
}
