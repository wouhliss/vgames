//! The social core for the active server (A4-T07): connection, friends, blocks, profiles,
//! settings and presence. Commands call [`SocialService`]; realtime events and REST
//! resyncs update its state and reach the UI through [`SocialEvents`]. Messaging (A4-T08)
//! lives in [`super::messaging`] and shares this state.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use time::OffsetDateTime;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_proto::realtime::{
    DeviceEvent, FriendEvent, PresenceChanged, Typing, TypingStart, kinds,
};
use vgames_proto::social::{PresenceUpdate, normalize_friend_code};

use super::api::{SocialApi, http_client};
use super::messaging::Work;
use super::model::{
    BlockedUser, Conversation, DeviceNotice, Friend, FriendCode, FriendList, FriendState,
    FriendTarget, Message, MessageStatus, Presence, SocialConnection, SocialError, SocialSettings,
    UserSummary,
};
use super::ports::{IdleSource, ServerSession, SessionSlot};
use super::presence;
use super::realtime::{self, Incoming, RealtimeHandle, Timing};
use super::store::{self, Keys, StoreError};
use crate::db::{Db, settings};
use crate::events::{AppEvent, EventBus};

/// Where UI notifications go (Tauri events in the app, a recorder in tests).
pub trait SocialEvents: Send + Sync + 'static {
    fn connection_changed(&self, connection: &SocialConnection);
    fn friends_changed(&self, friends: &FriendList);
    fn presence_changed(&self, user_id: Uuid, presence: &Presence);
    fn friend_request_received(&self, user: &UserSummary);
    /// The conversation list (order, last message, unread counts) changed.
    fn conversations_changed(&self, _conversations: &[Conversation]) {}
    /// A message was received or a notice was added to a conversation.
    fn message_received(&self, _message: &Message) {}
    /// One of your messages was sent or failed.
    fn message_status_changed(
        &self,
        _message_id: Uuid,
        _conversation_id: Uuid,
        _status: MessageStatus,
    ) {
    }
    /// A contact's devices changed (`conversation_id` is `None` when you share no conversation).
    fn device_notice(&self, _conversation_id: Option<Uuid>, _notice: &DeviceNotice) {}
    /// Someone is typing in a conversation (show it for 5 s).
    fn typing(&self, _conversation_id: Uuid, _user_id: Uuid) {}
}

/// At most one `typing` frame per conversation in this interval.
const TYPING_INTERVAL: Duration = Duration::from_secs(3);

/// The persisted social settings.
pub struct SocialSettingsKey;
impl settings::Setting for SocialSettingsKey {
    const KEY: &'static str = "social.settings";
    type Value = SocialSettings;
    fn default_value() -> SocialSettings {
        SocialSettings::default()
    }
}

fn rfc3339(t: OffsetDateTime) -> Option<String> {
    t.format(&time::format_description::well_known::Rfc3339)
        .ok()
}

fn presence_from(p: vgames_proto::social::Presence) -> Presence {
    Presence {
        status: p.status.into(),
        package_id: p.package_id,
        package_title: p.package_title,
        updated_at: p.updated_at.and_then(rfc3339),
    }
}

fn friend_from(f: vgames_proto::social::Friend) -> Friend {
    use vgames_proto::social::FriendState as S;
    Friend {
        user: f.user.into(),
        state: match f.state {
            S::Accepted => FriendState::Accepted,
            S::Incoming => FriendState::Incoming,
            S::Outgoing => FriendState::Outgoing,
        },
        presence: f.presence.map(presence_from),
        since: f.since.and_then(rfc3339),
    }
}

fn list_from(l: vgames_proto::social::FriendList) -> FriendList {
    FriendList {
        friends: l.friends.into_iter().map(friend_from).collect(),
        incoming: l.incoming.into_iter().map(friend_from).collect(),
        outgoing: l.outgoing.into_iter().map(friend_from).collect(),
    }
}

pub(super) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Maps a local store failure to a command error (`context` is logged, never shown).
pub(super) fn store_error(context: &str, e: StoreError) -> SocialError {
    match e {
        StoreError::NoAccount => SocialError::NotSignedIn,
        other => SocialError::internal(context, &other),
    }
}

pub(super) struct Inner {
    pub(super) sessions: SessionSlot,
    http: reqwest::Client,
    pub(super) db: Db,
    pub(super) events: Arc<dyn SocialEvents>,
    realtime: RealtimeHandle,
    idle: Arc<dyn IdleSource>,
    /// The friend list of the server it was fetched from.
    friends: Mutex<Option<(Uuid, FriendList)>>,
    inputs: Mutex<presence::Inputs>,
    /// What was last sent, and to which server.
    sent: Mutex<Option<(Uuid, PresenceUpdate)>>,
    /// Keys for the Olm pickles and the message store, once loaded from the OS secret store
    /// ([`SocialService::provide_keys`]). Until then friends and presence work and messaging
    /// commands report an error.
    pub(super) keys: OnceLock<Arc<Keys>>,
    /// Work queued for the messaging loop, and its wake-up.
    pub(super) work: Mutex<Work>,
    pub(super) wake: Notify,
    /// The server this device finished messaging setup on.
    pub(super) device: Mutex<Option<Uuid>>,
    /// When `typing` was last sent, per conversation.
    typing_sent: Mutex<HashMap<Uuid, Instant>>,
}

/// Cheap to clone; every clone talks to the same state.
#[derive(Clone)]
pub struct SocialService {
    pub(super) inner: Arc<Inner>,
}

impl SocialService {
    /// Starts the connection, event, presence and messaging tasks; they stop when `shutdown`
    /// fires. Messaging waits for [`Self::provide_keys`].
    pub async fn start(
        sessions: SessionSlot,
        db: Db,
        bus: &EventBus,
        events: Arc<dyn SocialEvents>,
        idle: Arc<dyn IdleSource>,
        timing: Timing,
        shutdown: CancellationToken,
    ) -> Result<Self, SocialError> {
        let http =
            http_client().map_err(|e| SocialError::internal("creating the HTTP client", &e))?;
        let settings = db
            .call(|c| settings::get::<SocialSettingsKey>(c))
            .await
            .map_err(|e| SocialError::internal("reading social settings", &e))?;
        let (tx, rx) = mpsc::channel(256);
        let realtime =
            realtime::spawn(sessions.clone(), http.clone(), timing, tx, shutdown.clone());
        let service = Self {
            inner: Arc::new(Inner {
                sessions,
                http,
                db,
                events,
                realtime,
                idle,
                friends: Mutex::new(None),
                inputs: Mutex::new(presence::Inputs {
                    show_current_game: settings.show_current_game,
                    ..presence::Inputs::default()
                }),
                sent: Mutex::new(None),
                keys: OnceLock::new(),
                work: Mutex::new(Work::default()),
                wake: Notify::new(),
                device: Mutex::new(None),
                typing_sent: Mutex::new(HashMap::new()),
            }),
        };
        tokio::spawn(service.clone().incoming_loop(rx, shutdown.clone()));
        tokio::spawn(service.clone().connection_loop(shutdown.clone()));
        tokio::spawn(service.clone().presence_loop(
            bus.subscribe(),
            timing.idle_poll,
            shutdown.clone(),
        ));
        tokio::spawn(service.clone().messaging_loop(shutdown.clone()));
        tokio::spawn(service.clone().network_loop(timing.network_poll, shutdown));
        Ok(service)
    }

    /// Hands over the message-store keys (loaded off the async runtime: keychains can block)
    /// and starts messaging setup. Later calls are ignored.
    pub fn provide_keys(&self, keys: Arc<Keys>) {
        if self.inner.keys.set(keys).is_ok() {
            self.queue(|w| w.setup = true);
        }
    }

    /// Where the session manager puts the current session.
    pub fn sessions(&self) -> &SessionSlot {
        &self.inner.sessions
    }

    pub fn connection(&self) -> SocialConnection {
        self.inner.realtime.connection()
    }

    /// The OS reported a network change.
    pub fn network_changed(&self) {
        self.inner.realtime.network_changed();
    }

    fn api(&self) -> Result<SocialApi<'_>, SocialError> {
        let session = self
            .inner
            .sessions
            .current()
            .ok_or(SocialError::NotSignedIn)?;
        Ok(self.api_for(session))
    }

    pub(super) fn api_for(&self, session: ServerSession) -> SocialApi<'_> {
        SocialApi {
            http: &self.inner.http,
            session,
            sessions: &self.inner.sessions,
        }
    }

    /// Runs `f` on the database thread (`context` names the step in logs).
    pub(super) async fn with_store<T, F>(
        &self,
        context: &'static str,
        f: F,
    ) -> Result<T, SocialError>
    where
        F: FnOnce(&mut rusqlite::Connection) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        self.inner
            .db
            .call(move |c| Ok(f(c)))
            .await
            .map_err(|e| SocialError::internal(context, &e))?
            .map_err(|e| store_error(context, e))
    }

    // ---- friends --------------------------------------------------------------------------

    /// Fetches the friend list, caches it and tells the UI.
    pub async fn friends_list(&self) -> Result<FriendList, SocialError> {
        let api = self.api()?;
        let server = api.session.server_id;
        let list = list_from(api.friends().await?);
        *lock(&self.inner.friends) = Some((server, list.clone()));
        self.inner.events.friends_changed(&list);
        Ok(list)
    }

    /// Resyncs in the background of an action; failures are only logged.
    async fn resync(&self) {
        if let Err(error) = self.friends_list().await {
            tracing::debug!(%error, "friend list resync failed");
        }
    }

    pub async fn friend_code_create(&self) -> Result<FriendCode, SocialError> {
        let code = self.api()?.friend_code().await?;
        Ok(FriendCode {
            code: code.code,
            expires_at: rfc3339(code.expires_at).unwrap_or_default(),
        })
    }

    pub async fn friend_request_send(&self, target: FriendTarget) -> Result<Friend, SocialError> {
        let api = self.api()?;
        let friend = match target {
            FriendTarget::Code { code } => {
                let code = normalize_friend_code(&code).ok_or_else(|| {
                    SocialError::invalid("code", "Friend codes are 8 letters and digits")
                })?;
                api.request_by_code(&code).await?
            }
            FriendTarget::User { user_id } => api.request_by_user(user_id).await?,
        };
        self.resync().await;
        Ok(friend_from(friend))
    }

    pub async fn friend_accept(&self, user_id: Uuid) -> Result<Friend, SocialError> {
        let friend = self.api()?.accept(user_id).await?;
        self.resync().await;
        Ok(friend_from(friend))
    }

    pub async fn friend_decline(&self, user_id: Uuid) -> Result<(), SocialError> {
        self.api()?.decline(user_id).await?;
        self.resync().await;
        Ok(())
    }

    pub async fn friend_remove(&self, user_id: Uuid) -> Result<(), SocialError> {
        self.api()?.remove(user_id).await?;
        self.resync().await;
        Ok(())
    }

    // ---- blocks and profiles ----------------------------------------------------------------

    pub async fn user_block(&self, user_id: Uuid) -> Result<(), SocialError> {
        let api = self.api()?;
        let server = api.session.server_id;
        let username = self.known_username(server, user_id);
        api.block(user_id).await?;
        let blocked_at = OffsetDateTime::now_utc().unix_timestamp();
        self.with_store("saving a block", move |c| {
            store::block_add(c, server, user_id, username.as_deref(), blocked_at)
        })
        .await?;
        self.resync().await;
        Ok(())
    }

    pub async fn user_unblock(&self, user_id: Uuid) -> Result<(), SocialError> {
        let api = self.api()?;
        let server = api.session.server_id;
        match api.unblock(user_id).await {
            // Already gone on the server: forget it here too.
            Ok(()) | Err(SocialError::NotFound) => {}
            Err(e) => return Err(e),
        }
        self.with_store("removing a block", move |c| {
            store::block_remove(c, server, user_id)
        })
        .await?;
        Ok(())
    }

    /// Users blocked from this install on the active server, newest first.
    pub async fn blocks_list(&self) -> Result<Vec<BlockedUser>, SocialError> {
        let server = self
            .inner
            .sessions
            .current()
            .ok_or(SocialError::NotSignedIn)?
            .server_id;
        let rows = self
            .with_store("reading blocks", move |c| store::blocks_list(c, server))
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(user_id, username, at)| {
                Some(BlockedUser {
                    user_id,
                    username,
                    blocked_at: OffsetDateTime::from_unix_timestamp(at)
                        .ok()
                        .and_then(rfc3339)?,
                })
            })
            .collect())
    }

    pub async fn user_profile(&self, user_id: Uuid) -> Result<UserSummary, SocialError> {
        Ok(self.api()?.profile(user_id).await?.into())
    }

    fn known_username(&self, server: Uuid, user_id: Uuid) -> Option<String> {
        let friends = lock(&self.inner.friends);
        let (s, list) = friends.as_ref()?;
        if *s != server {
            return None;
        }
        list.friends
            .iter()
            .chain(&list.incoming)
            .chain(&list.outgoing)
            .find(|f| f.user.id == user_id)
            .map(|f| f.user.username.clone())
    }

    // ---- settings ---------------------------------------------------------------------------

    pub async fn settings_get(&self) -> Result<SocialSettings, SocialError> {
        self.inner
            .db
            .call(|c| settings::get::<SocialSettingsKey>(c))
            .await
            .map_err(|e| SocialError::internal("reading social settings", &e))
    }

    pub async fn settings_set(&self, value: SocialSettings) -> Result<SocialSettings, SocialError> {
        let hotkey = value.overlay_hotkey.trim();
        if hotkey.is_empty() || hotkey.chars().count() > 64 || hotkey.chars().any(char::is_control)
        {
            return Err(SocialError::invalid(
                "overlay_hotkey",
                "Choose a key combination",
            ));
        }
        let value = SocialSettings {
            overlay_hotkey: hotkey.to_owned(),
            ..value
        };
        let stored = value.clone();
        self.inner
            .db
            .call(move |c| settings::set::<SocialSettingsKey>(c, &stored))
            .await
            .map_err(|e| SocialError::internal("saving social settings", &e))?;
        lock(&self.inner.inputs).show_current_game = value.show_current_game;
        self.publish_presence();
        Ok(value)
    }

    // ---- typing -----------------------------------------------------------------------------

    /// Tells the conversation's other members you are typing (best effort, throttled).
    pub fn typing_start(&self, conversation_id: Uuid) -> Result<(), SocialError> {
        self.inner
            .sessions
            .current()
            .ok_or(SocialError::NotSignedIn)?;
        let now = Instant::now();
        {
            let mut sent = lock(&self.inner.typing_sent);
            if sent
                .get(&conversation_id)
                .is_some_and(|at| now.duration_since(*at) < TYPING_INTERVAL)
            {
                return Ok(());
            }
            sent.retain(|_, at| now.duration_since(*at) < TYPING_INTERVAL);
            sent.insert(conversation_id, now);
        }
        if let Ok(data) = serde_json::to_value(TypingStart { conversation_id }) {
            self.inner.realtime.send(kinds::TYPING, data);
        }
        Ok(())
    }

    // ---- presence ---------------------------------------------------------------------------

    /// Sends `presence.set` if what we should show differs from what was last sent.
    fn publish_presence(&self) {
        let connection = self.inner.realtime.connection();
        let Some(server) = connection
            .server_id
            .filter(|_| connection.state == super::model::SocialConnectionState::Connected)
        else {
            return;
        };
        let desired = presence::desired(&lock(&self.inner.inputs), server);
        let mut sent = lock(&self.inner.sent);
        if sent.as_ref() == Some(&(server, desired.clone())) {
            return;
        }
        let Ok(data) = serde_json::to_value(&desired) else {
            return;
        };
        self.inner.realtime.send(kinds::PRESENCE_SET, data);
        *sent = Some((server, desired));
    }

    async fn presence_loop(
        self,
        mut bus: tokio::sync::broadcast::Receiver<AppEvent>,
        idle_poll: std::time::Duration,
        shutdown: CancellationToken,
    ) {
        let mut poll = tokio::time::interval(idle_poll);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = poll.tick() => {
                    lock(&self.inner.inputs).idle = self.inner.idle.idle_for();
                }
                event = bus.recv() => match event {
                    Ok(AppEvent::GameStarted(g)) => lock(&self.inner.inputs).game_started(g.package),
                    Ok(AppEvent::GameStopped(g)) => lock(&self.inner.inputs).game_stopped(g.package),
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "presence missed bus events");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
            }
            self.publish_presence();
        }
    }

    // ---- realtime ---------------------------------------------------------------------------

    /// Reconnects at once when the local route changes (only sampled while signed in).
    async fn network_loop(self, every: std::time::Duration, shutdown: CancellationToken) {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut watch = super::netwatch::Watch::default();
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = tick.tick() => {}
            }
            if self.inner.sessions.current().is_none() {
                continue;
            }
            if watch.changed(super::netwatch::sample()) {
                tracing::info!("network changed");
                self.inner.realtime.network_changed();
            }
        }
    }

    async fn connection_loop(self, shutdown: CancellationToken) {
        let mut watch = self.inner.realtime.watch();
        loop {
            let current = watch.borrow_and_update().clone();
            self.inner.events.connection_changed(&current);
            tokio::select! {
                () = shutdown.cancelled() => return,
                r = watch.changed() => if r.is_err() { return },
            }
        }
    }

    async fn incoming_loop(self, mut rx: mpsc::Receiver<Incoming>, shutdown: CancellationToken) {
        loop {
            let event = tokio::select! {
                () = shutdown.cancelled() => return,
                e = rx.recv() => match e { Some(e) => e, None => return },
            };
            self.handle(event).await;
        }
    }

    async fn handle(&self, event: Incoming) {
        match event {
            Incoming::Connected { .. } => {
                *lock(&self.inner.sent) = None;
                self.publish_presence();
                self.queue(|w| w.setup = true);
                self.resync().await;
            }
            Incoming::Event {
                server_id,
                kind,
                data,
            } => match kind.as_str() {
                kinds::PRESENCE_CHANGED => {
                    let Ok(p) = serde_json::from_value::<PresenceChanged>(data) else {
                        return;
                    };
                    let presence = Presence {
                        status: p.status.into(),
                        package_id: p.package_id,
                        package_title: p.package_title,
                        updated_at: rfc3339(OffsetDateTime::now_utc()),
                    };
                    if let Some((s, list)) = lock(&self.inner.friends).as_mut()
                        && *s == server_id
                        && let Some(f) = list.friends.iter_mut().find(|f| f.user.id == p.user_id)
                    {
                        f.presence = Some(presence.clone());
                    }
                    self.inner.events.presence_changed(p.user_id, &presence);
                }
                kinds::FRIEND_REQUEST => {
                    let from = serde_json::from_value::<FriendEvent>(data)
                        .ok()
                        .map(|e| e.user_id);
                    self.resync().await;
                    let user = from.and_then(|id| {
                        let friends = lock(&self.inner.friends);
                        let (_, list) = friends.as_ref()?;
                        list.incoming
                            .iter()
                            .find(|f| f.user.id == id)
                            .map(|f| f.user.clone())
                    });
                    if let Some(user) = user {
                        self.inner.events.friend_request_received(&user);
                    }
                }
                kinds::FRIEND_ACCEPTED | kinds::FRIEND_REMOVED => self.resync().await,
                kinds::INBOX_NEW => self.queue(|w| w.inbox = true),
                kinds::TYPING => {
                    if let Ok(t) = serde_json::from_value::<Typing>(data) {
                        self.inner.events.typing(t.conversation_id, t.user_id);
                    }
                }
                kinds::DEVICE_ADDED => {
                    if let Ok(e) = serde_json::from_value::<DeviceEvent>(data) {
                        self.queue(|w| w.users(server_id, e.user_id));
                    }
                }
                kinds::DEVICE_REVOKED => {
                    if let Ok(e) = serde_json::from_value::<DeviceEvent>(data) {
                        self.queue(|w| w.revoked(server_id, e.user_id, e.device_id));
                    }
                }
                _ => {}
            },
        }
    }
}

#[cfg(test)]
mod tests;
