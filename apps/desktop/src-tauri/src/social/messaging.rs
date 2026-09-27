//! End-to-end encrypted messaging on the active server (05-social §4.1–§4.2, A4-T08).
//!
//! One work loop per launcher runs, in order, whatever is queued:
//! 1. **setup** (after every `hello`): the Olm account exists, the device is registered (or
//!    the new session bound to it), and one-time keys are topped up below 20;
//! 2. **conversations**: the server's list, stored locally for members and ordering;
//! 3. **inbox**: every envelope for this device is decrypted, stored and acknowledged. An
//!    unknown sender triggers a device refresh and one retry; envelopes that can never be
//!    read are acknowledged and dropped so they do not block the queue;
//! 4. **device changes** (`device.added` / `device.revoked`): pins update and notices appear
//!    in the conversations with that user;
//! 5. **outbox**: each due message is encrypted for every device of every member and the
//!    user's own other devices (claiming keys for devices without a session), sent, and
//!    re-sent for the `unknown_devices` the server reports. A changed key blocks the message
//!    until the user trusts the device again.
//!
//! Plaintext only exists in memory here, and encrypted at rest in the local store.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_proto::social::{
    ConversationCreate, DeviceRegister, InboxEnvelope, MAX_ENVELOPES_PER_SEND, MAX_PARTY_MEMBERS,
    OsFamily, OutgoingEnvelope, SendMessageRequest,
};

use super::api::SocialApi;
use super::crypto::{INITIAL_ONE_TIME_KEYS, ONE_TIME_KEY_LOW_WATER};
use super::model::{
    ContactSecurity, Conversation, ConversationKind, DeviceNotice, DevicePlatform, Message,
    MessageBody, MessageStatus, MyDevice, SocialError, SocialLimit, UserSummary, unix_rfc3339,
};
use super::payload::{MAX_TEXT_CHARS, Payload};
use super::ports::ServerSession;
use super::service::{SocialService, lock};
use super::store::{self, Keys, PinState, ReceiveError, Received, RetryOutcome, StoreError};

/// Messages sent per outbox pass.
const OUTBOX_BATCH: u32 = 20;
/// Rounds of "encrypt for the devices the server says we missed".
const UNKNOWN_DEVICE_ROUNDS: usize = 2;
/// Envelope ids per `POST /v1/inbox/ack` (the server's limit).
const ACK_CHUNK: usize = 500;
/// How long the loop sleeps when no message waits for a retry.
const IDLE: Duration = Duration::from_secs(3600);

/// What the messaging loop still has to do.
#[derive(Debug, Default)]
pub(crate) struct Work {
    pub(super) setup: bool,
    pub(super) conversations: bool,
    pub(super) inbox: bool,
    pub(super) outbox: bool,
    /// (server, user) whose devices changed.
    users: HashSet<(Uuid, Uuid)>,
    /// (server, user, device) revoked.
    revoked: Vec<(Uuid, Uuid, Uuid)>,
}

impl Work {
    pub(super) fn users(&mut self, server: Uuid, user: Uuid) {
        self.users.insert((server, user));
    }

    pub(super) fn revoked(&mut self, server: Uuid, user: Uuid, device: Uuid) {
        self.revoked.push((server, user, device));
    }

    fn is_empty(&self) -> bool {
        !self.setup
            && !self.conversations
            && !self.inbox
            && !self.outbox
            && self.users.is_empty()
            && self.revoked.is_empty()
    }
}

fn now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn current_platform() -> OsFamily {
    if cfg!(windows) {
        OsFamily::Windows
    } else if cfg!(target_os = "macos") {
        OsFamily::Macos
    } else {
        OsFamily::Linux
    }
}

/// "vgames on <host name>", shown in the user's device list (control characters removed).
fn device_name() -> String {
    let host: String = sysinfo::System::host_name()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect();
    match host.trim() {
        "" => "vgames launcher".to_owned(),
        host => format!("vgames on {host}"),
    }
}

fn conversation_from(c: vgames_proto::social::Conversation) -> (Conversation, i64) {
    let created = c.created_at.unix_timestamp();
    let conversation = Conversation {
        id: c.id,
        kind: match c.kind {
            vgames_proto::social::ConversationKind::Direct => ConversationKind::Direct,
            vgames_proto::social::ConversationKind::Party => ConversationKind::Party,
        },
        members: c.members.into_iter().map(UserSummary::from).collect(),
        last_message: None,
        unread: 0,
        created_at: unix_rfc3339(created),
    };
    (conversation, created)
}

/// Failures that retrying cannot fix: the message fails until the user retries it.
fn is_permanent(e: &SocialError) -> bool {
    matches!(
        e,
        SocialError::KeyChanged { .. }
            | SocialError::NotFound
            | SocialError::InvalidInput { .. }
            | SocialError::LimitReached { .. }
    )
}

/// Failures that stop the whole outbox pass: the next message would fail the same way.
fn stops_pass(e: &SocialError) -> bool {
    matches!(
        e,
        SocialError::Offline | SocialError::RateLimited { .. } | SocialError::NotSignedIn
    )
}

/// `not_allowed` (not a friend, blocked) looks like an unknown user to the UI.
fn hide_not_allowed(e: SocialError) -> SocialError {
    match e {
        SocialError::Server { code, .. } if code == "not_allowed" => SocialError::NotFound,
        other => other,
    }
}

impl SocialService {
    fn keys(&self) -> Result<Arc<Keys>, SocialError> {
        self.inner
            .keys
            .get()
            .cloned()
            .ok_or_else(|| SocialError::Internal {
                detail: "secure storage for messages is not available".into(),
            })
    }

    fn session(&self) -> Result<ServerSession, SocialError> {
        self.inner
            .sessions
            .current()
            .ok_or(SocialError::NotSignedIn)
    }

    /// Queues work for the messaging loop and wakes it.
    pub(super) fn queue(&self, f: impl FnOnce(&mut Work)) {
        f(&mut lock(&self.inner.work));
        self.inner.wake.notify_one();
    }

    pub(super) async fn messaging_loop(self, shutdown: CancellationToken) {
        loop {
            let due = self.outbox_due_in().await;
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = self.inner.wake.notified() => {}
                () = tokio::time::sleep(due) => lock(&self.inner.work).outbox = true,
            }
            let work = std::mem::take(&mut *lock(&self.inner.work));
            if work.is_empty() {
                continue;
            }
            if let Err(error) = self.run(work).await {
                tracing::debug!(%error, "messaging pass stopped");
            }
        }
    }

    /// Time until the next outbox retry.
    async fn outbox_due_in(&self) -> Duration {
        let Some(server) = self.inner.sessions.current().map(|s| s.server_id) else {
            return IDLE;
        };
        match self
            .with_store("reading the outbox", move |c| {
                store::outbox_next_due(c, server)
            })
            .await
        {
            Ok(Some(at)) => {
                Duration::from_secs(u64::try_from(at.saturating_sub(now())).unwrap_or(0)).min(IDLE)
            }
            Ok(None) | Err(_) => IDLE,
        }
    }

    async fn run(&self, work: Work) -> Result<(), SocialError> {
        let session = self.session()?;
        let server = session.server_id;
        let ready = *lock(&self.inner.device) == Some(server);
        if work.setup || !ready {
            // On failure everything waits for the next hello (or the next queued work).
            self.setup(&session).await?;
        }
        if work.conversations {
            self.sync_conversations(&session).await?;
        }
        if work.inbox {
            self.sync_inbox(&session).await?;
        }
        // Device events queued for another server no longer apply.
        for (_, user, device) in work.revoked.into_iter().filter(|r| r.0 == server) {
            self.device_revoked(&session, user, device).await?;
        }
        for (_, user) in work.users.into_iter().filter(|u| u.0 == server) {
            self.refresh_devices(&session, user).await?;
        }
        if work.outbox {
            self.flush_outbox(&session).await?;
        }
        Ok(())
    }

    // ---- setup ------------------------------------------------------------------------------

    async fn setup(&self, session: &ServerSession) -> Result<(), SocialError> {
        let keys = self.keys()?;
        let api = self.api_for(session.clone());
        let (server, user) = (session.server_id, session.user_id);
        let k = keys.clone();
        self.with_store("creating the messaging account", move |c| {
            store::ensure_account(c, &k, server, user, now())
        })
        .await?;
        let device = match self.register(&api, &keys, server).await {
            Err(SocialError::Conflict { code, .. }) if code == "device_keys_in_use" => {
                // This device was revoked (which also signed it out): continue as a new
                // device. Local history and blocks stay; only the Olm state is replaced.
                tracing::warn!(%server, "this device was revoked; creating new messaging keys");
                let k = keys.clone();
                self.with_store("resetting the messaging account", move |c| {
                    store::reset_account(c, server)?;
                    store::ensure_account(c, &k, server, user, now()).map(drop)
                })
                .await?;
                self.register(&api, &keys, server).await?
            }
            other => other?,
        };
        self.top_up_keys(&api, &keys, server, device).await?;
        *lock(&self.inner.device) = Some(server);
        self.queue(|w| {
            w.conversations = true;
            w.inbox = true;
            w.outbox = true;
        });
        Ok(())
    }

    /// Registers the device (or binds this session to it) and returns its id.
    async fn register(
        &self,
        api: &SocialApi<'_>,
        keys: &Arc<Keys>,
        server: Uuid,
    ) -> Result<Uuid, SocialError> {
        let k = keys.clone();
        let signed = self
            .with_store("reading device keys", move |c| {
                store::signed_device_keys(c, &k, server)
            })
            .await?;
        let device = api
            .register_device(&DeviceRegister {
                display_name: device_name(),
                platform: current_platform(),
                identity_key: signed.identity_key,
                signing_key: signed.signing_key,
                keys_signature: signed.keys_signature,
            })
            .await?;
        let id = device.id;
        self.with_store("saving the device id", move |c| {
            store::set_device_id(c, server, id, now())
        })
        .await?;
        Ok(id)
    }

    /// Uploads keys when fewer than 20 one-time keys remain or the fallback key is missing.
    async fn top_up_keys(
        &self,
        api: &SocialApi<'_>,
        keys: &Arc<Keys>,
        server: Uuid,
        device: Uuid,
    ) -> Result<(), SocialError> {
        let mine = api.my_devices().await?;
        let Some(current) = mine.items.iter().find(|d| d.id == device) else {
            return Err(SocialError::NotFound);
        };
        let available = usize::try_from(current.one_time_keys_available.unwrap_or(0)).unwrap_or(0);
        let has_fallback = current.has_fallback_key.unwrap_or(false);
        if available >= ONE_TIME_KEY_LOW_WATER && has_fallback {
            return Ok(());
        }
        let new = INITIAL_ONE_TIME_KEYS.saturating_sub(available);
        let k = keys.clone();
        let upload = self
            .with_store("preparing one-time keys", move |c| {
                store::prepare_key_upload(c, &k, server, new, !has_fallback, now())
            })
            .await?;
        api.upload_keys(device, &upload).await?;
        let k = keys.clone();
        self.with_store("marking keys published", move |c| {
            store::mark_keys_published(c, &k, server, now())
        })
        .await?;
        tracing::debug!(uploaded = new, "one-time keys topped up");
        Ok(())
    }

    // ---- conversations ----------------------------------------------------------------------

    async fn sync_conversations(
        &self,
        session: &ServerSession,
    ) -> Result<Vec<Conversation>, SocialError> {
        let keys = self.keys()?;
        let rows: Vec<_> = self
            .api_for(session.clone())
            .conversations()
            .await?
            .into_iter()
            .map(conversation_from)
            .collect();
        let server = session.server_id;
        let all = self
            .with_store("storing conversations", move |c| {
                for (conversation, created) in &rows {
                    store::upsert_conversation(c, server, conversation, *created)?;
                }
                store::list_conversations(c, &keys, server)
            })
            .await?;
        self.inner.events.conversations_changed(&all);
        Ok(all)
    }

    async fn upsert_one(
        &self,
        session: &ServerSession,
        c: vgames_proto::social::Conversation,
    ) -> Result<Conversation, SocialError> {
        let keys = self.keys()?;
        let server = session.server_id;
        let (conversation, created) = conversation_from(c);
        let id = conversation.id;
        let (one, all) = self
            .with_store("storing a conversation", move |db| {
                store::upsert_conversation(db, server, &conversation, created)?;
                let one = store::get_conversation(db, &keys, server, id)?;
                Ok((one, store::list_conversations(db, &keys, server)?))
            })
            .await?;
        self.inner.events.conversations_changed(&all);
        one.ok_or(SocialError::NotFound)
    }

    async fn emit_conversations(&self, server: Uuid) {
        let Ok(keys) = self.keys() else { return };
        if let Ok(all) = self
            .with_store("reading conversations", move |c| {
                store::list_conversations(c, &keys, server)
            })
            .await
        {
            self.inner.events.conversations_changed(&all);
        }
    }

    async fn is_known_conversation(&self, server: Uuid, conversation: Uuid) -> bool {
        self.with_store("reading members", move |c| {
            store::conversation_members(c, server, conversation)
        })
        .await
        .is_ok_and(|m| m.is_some())
    }

    // ---- receiving --------------------------------------------------------------------------

    async fn receive_one(
        &self,
        session: &ServerSession,
        env: &InboxEnvelope,
    ) -> Result<Received, ReceiveError> {
        let keys = self
            .inner
            .keys
            .get()
            .cloned()
            .ok_or(ReceiveError::Store(StoreError::NoAccount))?;
        let (server, me, env) = (session.server_id, session.user_id, env.clone());
        match self
            .inner
            .db
            .call(move |c| Ok(store::receive(c, &keys, server, me, &env, now())))
            .await
        {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!(%error, "the database is unavailable");
                Err(ReceiveError::Store(StoreError::Corrupt))
            }
        }
    }

    async fn sync_inbox(&self, session: &ServerSession) -> Result<(), SocialError> {
        let api = self.api_for(session.clone());
        let server = session.server_id;
        let mut cursor: Option<String> = None;
        let mut new_conversation = false;
        loop {
            let page = api.inbox(cursor.as_deref()).await?;
            let mut acks = Vec::new();
            let mut stop = false;
            for env in &page.items {
                let mut attempt = self.receive_one(session, env).await;
                if let Err(
                    ReceiveError::UnknownSender { user_id, .. }
                    | ReceiveError::KeyMismatch { user_id, .. },
                ) = &attempt
                {
                    // A device we have not pinned yet (or whose key changed): fetch the
                    // sender's devices, which also posts the notice, then retry once.
                    let user = *user_id;
                    if self.refresh_devices(session, user).await.is_ok() {
                        attempt = self.receive_one(session, env).await;
                    }
                }
                match attempt {
                    Ok(received) => {
                        acks.push(env.id);
                        if let Some(m) = self.announce(&received)
                            && !self.is_known_conversation(server, m.conversation_id).await
                        {
                            new_conversation = true;
                        }
                    }
                    Err(ReceiveError::Store(error)) => {
                        // Keep this envelope on the server and stop: it is retried next time.
                        tracing::warn!(%error, "cannot store a received message");
                        stop = true;
                        break;
                    }
                    Err(error) => {
                        tracing::info!(envelope = %env.id, %error, "dropping an unreadable envelope");
                        let id = env.id;
                        if let Err(error) = self
                            .with_store("dropping an envelope", move |c| {
                                store::mark_envelope_processed(c, server, id, now())
                            })
                            .await
                        {
                            tracing::debug!(%error, "envelope not marked processed");
                        }
                        acks.push(env.id);
                    }
                }
            }
            for chunk in acks.chunks(ACK_CHUNK) {
                api.ack(chunk.to_vec()).await?;
            }
            match page.next_cursor {
                Some(next) if !stop => cursor = Some(next),
                _ => break,
            }
        }
        if new_conversation {
            self.sync_conversations(session).await?;
        } else {
            self.emit_conversations(server).await;
        }
        Ok(())
    }

    /// Tells the UI about a received message and returns it.
    fn announce(&self, received: &Received) -> Option<Message> {
        match received {
            Received::Message(message) | Received::InviteJoin { message, .. } => {
                self.inner.events.message_received(message);
                Some(message.clone())
            }
            Received::Receipt { .. } | Received::Duplicate => None,
        }
    }

    // ---- devices ----------------------------------------------------------------------------

    /// Fetches and pins `user`'s devices; posts notices for what changed.
    async fn refresh_devices(
        &self,
        session: &ServerSession,
        user: Uuid,
    ) -> Result<(), SocialError> {
        let keys = self.keys()?;
        let devices = match self.api_for(session.clone()).user_devices(user).await {
            Ok(list) => list.items,
            // No longer visible (unfriended, blocked): nothing to pin.
            Err(SocialError::NotFound) => Vec::new(),
            Err(e) => return Err(e),
        };
        let server = session.server_id;
        let changes = self
            .with_store("pinning devices", move |c| {
                store::sync_user_devices(c, &keys, server, user, &devices, now())
            })
            .await?;
        self.post_notices(session, user, changes.notices(user))
            .await;
        Ok(())
    }

    async fn device_revoked(
        &self,
        session: &ServerSession,
        user: Uuid,
        device: Uuid,
    ) -> Result<(), SocialError> {
        let server = session.server_id;
        let owner = self
            .with_store("revoking a device", move |c| {
                store::mark_device_revoked(c, server, device, now())
            })
            .await?;
        if owner == Some(user) {
            let notice = DeviceNotice::DeviceRevoked {
                user_id: user,
                device_id: device,
            };
            self.post_notices(session, user, vec![notice]).await;
        }
        Ok(())
    }

    /// Stores each notice in every conversation with `user` (never for yourself) and tells
    /// the UI.
    async fn post_notices(&self, session: &ServerSession, user: Uuid, notices: Vec<DeviceNotice>) {
        if notices.is_empty() || user == session.user_id {
            return;
        }
        let Ok(keys) = self.keys() else { return };
        let server = session.server_id;
        let stored = self
            .with_store("posting device notices", move |c| {
                let mut out = Vec::new();
                for conversation in store::list_conversations(c, &keys, server)?
                    .iter()
                    .filter(|cv| cv.members.iter().any(|m| m.id == user))
                {
                    for notice in &notices {
                        let message = store::insert_notice(
                            c,
                            &keys,
                            server,
                            conversation.id,
                            notice.clone(),
                            now(),
                        )?;
                        if message.is_some() {
                            out.push((Some(conversation.id), notice.clone(), message));
                        }
                    }
                }
                if out.is_empty() {
                    out.extend(notices.into_iter().map(|n| (None, n, None)));
                }
                Ok(out)
            })
            .await;
        match stored {
            Ok(stored) => {
                for (conversation, notice, message) in stored {
                    if let Some(m) = message {
                        self.inner.events.message_received(&m);
                    }
                    self.inner.events.device_notice(conversation, &notice);
                }
            }
            Err(error) => tracing::debug!(%error, "device notices not stored"),
        }
    }

    // ---- sending ----------------------------------------------------------------------------

    async fn flush_outbox(&self, session: &ServerSession) -> Result<(), SocialError> {
        let keys = self.keys()?;
        let server = session.server_id;
        let k = keys.clone();
        let items = self
            .with_store("reading the outbox", move |c| {
                store::outbox_due(c, &k, server, now(), OUTBOX_BATCH)
            })
            .await?;
        let full = items.len() == OUTBOX_BATCH as usize;
        for item in items {
            let cmid = item.client_message_id;
            match self.send_one(session, &keys, &item).await {
                Ok(()) => {
                    let sent = self
                        .with_store("updating the outbox", move |c| {
                            store::outbox_sent(c, server, cmid)
                        })
                        .await?;
                    if let Some((message, conversation)) = sent {
                        self.inner.events.message_status_changed(
                            message,
                            conversation,
                            MessageStatus::Sent,
                        );
                    }
                }
                Err(e) => {
                    tracing::info!(error = %e, "sending a message failed");
                    let (permanent, reason) = (is_permanent(&e), e.to_string());
                    let outcome = self
                        .with_store("updating the outbox", move |c| {
                            store::outbox_attempt_failed(c, server, cmid, now(), &reason, permanent)
                        })
                        .await?;
                    if let RetryOutcome::Failed {
                        message_id,
                        conversation_id,
                    } = outcome
                    {
                        self.inner.events.message_status_changed(
                            message_id,
                            conversation_id,
                            MessageStatus::Failed,
                        );
                    }
                    if stops_pass(&e) {
                        return Err(e);
                    }
                }
            }
        }
        if full {
            // More may be due: run another pass right away.
            self.queue(|w| w.outbox = true);
        }
        Ok(())
    }

    async fn send_one(
        &self,
        session: &ServerSession,
        keys: &Arc<Keys>,
        item: &store::OutboxItem,
    ) -> Result<(), SocialError> {
        let server = session.server_id;
        let conversation = item.conversation_id;
        let members = || {
            self.with_store("reading members", move |c| {
                store::conversation_members(c, server, conversation)
            })
        };
        let mut users = match members().await? {
            Some(users) => users,
            None => {
                self.sync_conversations(session).await?;
                members().await?.ok_or(SocialError::NotFound)?
            }
        };
        if !users.contains(&session.user_id) {
            users.push(session.user_id);
        }
        let mut targets = self.target_devices(session, &users).await?;
        let api = self.api_for(session.clone());
        let mut sent_to: HashSet<Uuid> = HashSet::new();
        for _ in 0..=UNKNOWN_DEVICE_ROUNDS {
            targets.retain(|d| !sent_to.contains(d));
            let envelopes = self
                .encrypt(session, keys, &targets, &item.plaintext)
                .await?;
            if envelopes.is_empty() && !sent_to.is_empty() {
                break;
            }
            sent_to.extend(envelopes.iter().map(|e| e.recipient_device_id));
            // Even with no envelope (nobody else has a device yet) one request goes out: the
            // server then lists the devices we missed. Larger fan-outs go in several requests
            // with the same id; the server answers for everything sent under it so far.
            let mut chunks: Vec<Vec<OutgoingEnvelope>> = envelopes
                .chunks(MAX_ENVELOPES_PER_SEND)
                .map(<[_]>::to_vec)
                .collect();
            if chunks.is_empty() {
                chunks.push(Vec::new());
            }
            let mut unknown_devices = Vec::new();
            for envelopes in chunks {
                unknown_devices = api
                    .send_message(
                        conversation,
                        &SendMessageRequest {
                            client_message_id: item.client_message_id,
                            envelopes,
                        },
                    )
                    .await?
                    .unknown_devices;
            }
            let unknown: HashSet<Uuid> = unknown_devices
                .into_iter()
                .filter(|d| !sent_to.contains(d))
                .collect();
            if unknown.is_empty() {
                return Ok(());
            }
            // Devices appeared since we last looked: pin them and encrypt for them too.
            for user in &users {
                self.refresh_devices(session, *user).await?;
            }
            targets = self.target_devices(session, &users).await?;
            targets.retain(|d| unknown.contains(d));
        }
        Ok(())
    }

    /// Non-revoked pinned devices of `users`, without this device. Users with no pinned
    /// device yet are fetched first.
    async fn target_devices(
        &self,
        session: &ServerSession,
        users: &[Uuid],
    ) -> Result<Vec<Uuid>, SocialError> {
        let server = session.server_id;
        let me = self.current_device(server).await?;
        let pinned = |users: Vec<Uuid>| {
            self.with_store("reading devices", move |c| {
                let mut out = Vec::new();
                for user in users {
                    out.push((user, store::pinned_devices(c, server, user)?));
                }
                Ok(out)
            })
        };
        let mut rows = pinned(users.to_vec()).await?;
        let unpinned: Vec<Uuid> = rows
            .iter()
            .filter(|(_, devices)| devices.is_empty())
            .map(|(user, _)| *user)
            .collect();
        if !unpinned.is_empty() {
            for user in &unpinned {
                self.refresh_devices(session, *user).await?;
            }
            rows = pinned(users.to_vec()).await?;
        }
        Ok(rows
            .into_iter()
            .flat_map(|(_, devices)| devices)
            .filter(|d| d.state != PinState::Revoked && Some(d.device.device_id) != me)
            .map(|d| d.device.device_id)
            .collect())
    }

    async fn current_device(&self, server: Uuid) -> Result<Option<Uuid>, SocialError> {
        let keys = self.keys()?;
        let account = self
            .with_store("reading the account", move |c| {
                store::account_info(c, &keys, server)
            })
            .await?;
        Ok(account.and_then(|a| a.device_id))
    }

    /// Encrypts for `devices`, claiming keys for those without a session. A changed key
    /// refuses the whole message.
    async fn encrypt(
        &self,
        session: &ServerSession,
        keys: &Arc<Keys>,
        devices: &[Uuid],
        plaintext: &[u8],
    ) -> Result<Vec<OutgoingEnvelope>, SocialError> {
        if devices.is_empty() {
            return Ok(Vec::new());
        }
        let server = session.server_id;
        let list = devices.to_vec();
        let missing = self
            .with_store("reading sessions", move |c| {
                store::devices_without_session(c, server, &list)
            })
            .await?;
        let claimed = if missing.is_empty() {
            Vec::new()
        } else {
            // The store verifies each key's signature against the pinned signing key.
            self.api_for(session.clone())
                .claim_keys(missing)
                .await?
                .items
        };
        let (k, list, bytes) = (keys.clone(), devices.to_vec(), plaintext.to_vec());
        let out = self
            .with_store("encrypting", move |c| {
                store::encrypt_for(c, &k, server, &list, &claimed, &bytes, now())
            })
            .await?;
        if let Some((user_id, _)) = out.blocked.first().copied() {
            let device_ids = out
                .blocked
                .iter()
                .filter(|(u, _)| *u == user_id)
                .map(|(_, d)| *d)
                .collect();
            return Err(SocialError::KeyChanged {
                user_id,
                device_ids,
            });
        }
        if !out.missing.is_empty() {
            tracing::info!(
                devices = out.missing.len(),
                "no usable key for some devices; they miss this message"
            );
        }
        Ok(out.envelopes)
    }

    // ---- commands ---------------------------------------------------------------------------

    /// Conversations, refreshed from the server when it can be reached.
    pub async fn conversations_list(&self) -> Result<Vec<Conversation>, SocialError> {
        let session = self.session()?;
        match self.sync_conversations(&session).await {
            Err(SocialError::Offline) => {
                let keys = self.keys()?;
                let server = session.server_id;
                self.with_store("reading conversations", move |c| {
                    store::list_conversations(c, &keys, server)
                })
                .await
            }
            other => other,
        }
    }

    /// Opens (or creates) the direct conversation with a friend.
    pub async fn conversation_open_direct(
        &self,
        user_id: Uuid,
    ) -> Result<Conversation, SocialError> {
        let session = self.session()?;
        let c = self
            .api_for(session.clone())
            .create_conversation(&ConversationCreate::Direct { user_id })
            .await
            .map_err(hide_not_allowed)?;
        self.upsert_one(&session, c).await
    }

    /// Creates a party chat with 1–15 friends.
    pub async fn conversation_create_party(
        &self,
        user_ids: Vec<Uuid>,
    ) -> Result<Conversation, SocialError> {
        if user_ids.is_empty() || user_ids.len() >= MAX_PARTY_MEMBERS {
            return Err(SocialError::LimitReached {
                limit: SocialLimit::PartyMembers,
            });
        }
        let session = self.session()?;
        let c = self
            .api_for(session.clone())
            .create_conversation(&ConversationCreate::Party { user_ids })
            .await
            .map_err(hide_not_allowed)?;
        self.upsert_one(&session, c).await
    }

    /// Messages of a conversation, newest first, before `before` (a message id).
    pub async fn messages_list(
        &self,
        conversation_id: Uuid,
        before: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<Message>, SocialError> {
        if !(1..=200).contains(&limit) {
            return Err(SocialError::invalid("limit", "1 to 200"));
        }
        let keys = self.keys()?;
        let server = self.session()?.server_id;
        self.with_store("reading messages", move |c| {
            store::list_messages(c, &keys, server, conversation_id, before, limit)
        })
        .await
    }

    /// Stores the message as `pending` and queues it; `message-status-changed` follows.
    pub async fn message_send(
        &self,
        conversation_id: Uuid,
        text: String,
    ) -> Result<Message, SocialError> {
        if text.trim().is_empty() || text.chars().count() > MAX_TEXT_CHARS {
            return Err(SocialError::invalid("text", "1 to 4000 characters"));
        }
        let keys = self.keys()?;
        let server = self.session()?.server_id;
        if !self.is_known_conversation(server, conversation_id).await {
            return Err(SocialError::NotFound);
        }
        let payload = Payload::text(
            conversation_id,
            Uuid::now_v7(),
            OffsetDateTime::now_utc(),
            &text,
        )
        .map_err(|_| SocialError::invalid("text", "1 to 4000 characters"))?;
        let message = self
            .with_store("storing a message", move |c| {
                store::create_outgoing(
                    c,
                    &keys,
                    server,
                    &payload,
                    MessageBody::Text { text },
                    now(),
                )
            })
            .await?;
        self.queue(|w| w.outbox = true);
        self.emit_conversations(server).await;
        Ok(message)
    }

    /// Queues a failed message again.
    pub async fn message_retry(&self, message_id: Uuid) -> Result<Message, SocialError> {
        let keys = self.keys()?;
        let server = self.session()?.server_id;
        let message = self
            .with_store("retrying a message", move |c| {
                store::message_retry(c, &keys, server, message_id, now())
            })
            .await?
            .ok_or(SocialError::NotFound)?;
        self.queue(|w| w.outbox = true);
        Ok(message)
    }

    pub async fn conversation_mark_read(&self, conversation_id: Uuid) -> Result<(), SocialError> {
        let server = self.session()?.server_id;
        self.with_store("marking read", move |c| {
            store::mark_read(c, server, conversation_id)
        })
        .await?;
        self.emit_conversations(server).await;
        Ok(())
    }

    /// Safety number and devices of a contact (devices refreshed first when online).
    pub async fn contact_security(&self, user_id: Uuid) -> Result<ContactSecurity, SocialError> {
        let session = self.session()?;
        if let Err(error) = self.refresh_devices(&session, user_id).await {
            tracing::debug!(%error, "showing the last known devices");
        }
        self.stored_contact_security(session.server_id, user_id)
            .await
    }

    async fn stored_contact_security(
        &self,
        server: Uuid,
        user_id: Uuid,
    ) -> Result<ContactSecurity, SocialError> {
        let keys = self.keys()?;
        self.with_store("computing the safety number", move |c| {
            store::contact_security(c, &keys, server, user_id)
        })
        .await
    }

    pub async fn contact_set_verified(
        &self,
        user_id: Uuid,
        verified: bool,
    ) -> Result<ContactSecurity, SocialError> {
        let keys = self.keys()?;
        let server = self.session()?.server_id;
        self.with_store("saving verification", move |c| {
            store::set_verified(c, &keys, server, user_id, verified, now())
        })
        .await
    }

    /// Accepts a changed key (after checking it with the contact). Failed messages can
    /// then be retried.
    pub async fn contact_trust_device(
        &self,
        user_id: Uuid,
        device_id: Uuid,
    ) -> Result<ContactSecurity, SocialError> {
        let server = self.session()?.server_id;
        let changed = self
            .with_store("trusting a device", move |c| {
                store::trust_device(c, server, user_id, device_id, now())
            })
            .await?;
        if !changed {
            return Err(SocialError::NotFound);
        }
        self.queue(|w| w.outbox = true);
        self.stored_contact_security(server, user_id).await
    }

    /// Your devices on the active server.
    pub async fn devices_list(&self) -> Result<Vec<MyDevice>, SocialError> {
        let session = self.session()?;
        let current = self.current_device(session.server_id).await?;
        let list = self.api_for(session).my_devices().await?;
        Ok(list
            .items
            .into_iter()
            .map(|d| MyDevice {
                current: Some(d.id) == current,
                id: d.id,
                display_name: d.display_name,
                platform: match d.platform {
                    OsFamily::Windows => DevicePlatform::Windows,
                    OsFamily::Linux => DevicePlatform::Linux,
                    OsFamily::Macos => DevicePlatform::Macos,
                },
                created_at: unix_rfc3339(d.created_at.unix_timestamp()),
                last_seen_at: d.last_seen_at.map(|t| unix_rfc3339(t.unix_timestamp())),
            })
            .collect())
    }

    /// Revokes one of your devices; it stops receiving messages and is signed out.
    pub async fn device_revoke(&self, device_id: Uuid) -> Result<(), SocialError> {
        let session = self.session()?;
        self.api_for(session.clone())
            .revoke_device(device_id)
            .await?;
        self.queue(|w| w.revoked(session.server_id, session.user_id, device_id));
        Ok(())
    }
}

#[cfg(test)]
mod tests;
