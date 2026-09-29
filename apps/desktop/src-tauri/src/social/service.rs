//! The social core for the active server (A4-T07): connection, friends, blocks, profiles,
//! settings and presence. Commands call [`SocialService`]; realtime events and REST
//! resyncs update its state and reach the UI through [`SocialEvents`].

use std::sync::{Arc, Mutex, PoisonError};

use time::OffsetDateTime;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_proto::realtime::{FriendEvent, PresenceChanged, kinds};
use vgames_proto::social::{PresenceUpdate, normalize_friend_code};

use super::api::{SocialApi, http_client};
use super::model::{
    BlockedUser, Conversation, DeviceNotice, Friend, FriendCode, FriendList, FriendState,
    FriendTarget, Invite, InviteInstallReason, Message, MessageStatus, Presence, SocialConnection,
    SocialError, SocialSettings, UserSummary,
};
use super::ports::{Games, LocalLibrary};
use super::ports::{IdleSource, SessionSlot};
use super::presence;
use super::realtime::{self, Incoming, RealtimeHandle, Timing};
use super::store::Keys;
use crate::db::{Db, settings};
use crate::events::PackageRef;
use crate::events::{AppEvent, EventBus};

mod invites;
mod messaging;
pub use messaging::{default_device_name, platform};

/// Where UI notifications go (Tauri events in the app, a recorder in tests).
pub trait SocialEvents: Send + Sync + 'static {
    fn connection_changed(&self, connection: &SocialConnection);
    fn friends_changed(&self, friends: &FriendList);
    fn presence_changed(&self, user_id: Uuid, presence: &Presence);
    fn friend_request_received(&self, user: &UserSummary);
    fn conversations_changed(&self, conversations: &[Conversation]);
    fn message_received(&self, message: &Message);
    fn message_status_changed(
        &self,
        message_id: Uuid,
        conversation_id: Uuid,
        status: MessageStatus,
    );
    fn typing(&self, conversation_id: Uuid, user_id: Uuid);
    fn device_notice(&self, conversation_id: Option<Uuid>, notice: &DeviceNotice);
    fn invite_received(&self, invite: &Invite);
    fn invite_changed(&self, invite: &Invite);
    fn invite_install_requested(
        &self,
        invite_id: Uuid,
        package: PackageRef,
        reason: InviteInstallReason,
    );
}

/// Sends every social event to several sinks (the WebView and the overlay hub).
pub struct FanOut(pub Vec<Arc<dyn SocialEvents>>);

impl SocialEvents for FanOut {
    fn connection_changed(&self, c: &SocialConnection) {
        self.0.iter().for_each(|s| s.connection_changed(c));
    }
    fn friends_changed(&self, f: &FriendList) {
        self.0.iter().for_each(|s| s.friends_changed(f));
    }
    fn presence_changed(&self, u: Uuid, p: &Presence) {
        self.0.iter().for_each(|s| s.presence_changed(u, p));
    }
    fn friend_request_received(&self, u: &UserSummary) {
        self.0.iter().for_each(|s| s.friend_request_received(u));
    }
    fn conversations_changed(&self, c: &[Conversation]) {
        self.0.iter().for_each(|s| s.conversations_changed(c));
    }
    fn message_received(&self, m: &Message) {
        self.0.iter().for_each(|s| s.message_received(m));
    }
    fn message_status_changed(&self, m: Uuid, c: Uuid, st: MessageStatus) {
        self.0
            .iter()
            .for_each(|s| s.message_status_changed(m, c, st));
    }
    fn typing(&self, c: Uuid, u: Uuid) {
        self.0.iter().for_each(|s| s.typing(c, u));
    }
    fn device_notice(&self, c: Option<Uuid>, n: &DeviceNotice) {
        self.0.iter().for_each(|s| s.device_notice(c, n));
    }
    fn invite_received(&self, i: &Invite) {
        self.0.iter().for_each(|s| s.invite_received(i));
    }
    fn invite_changed(&self, i: &Invite) {
        self.0.iter().for_each(|s| s.invite_changed(i));
    }
    fn invite_install_requested(&self, i: Uuid, p: PackageRef, r: InviteInstallReason) {
        self.0
            .iter()
            .for_each(|s| s.invite_install_requested(i, p, r));
    }
}

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

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Inner {
    sessions: SessionSlot,
    http: reqwest::Client,
    db: Db,
    events: Arc<dyn SocialEvents>,
    realtime: RealtimeHandle,
    idle: Arc<dyn IdleSource>,
    /// The friend list of the server it was fetched from.
    friends: Mutex<Option<(Uuid, FriendList)>>,
    inputs: Mutex<presence::Inputs>,
    /// What was last sent, and to which server.
    sent: Mutex<Option<(Uuid, PresenceUpdate)>>,
    /// Keychain keys for the Olm pickles and the history at rest.
    keys: Arc<Keys>,
    /// The name this install registers its chat device under.
    device_name: String,
    messaging: messaging::MessagingState,
    /// Library and launch port for invites (Agent 2's, once A2-T09 lands).
    games: Mutex<Arc<dyn Games>>,
}

/// Cheap to clone; every clone talks to the same state.
#[derive(Clone)]
pub struct SocialService {
    inner: Arc<Inner>,
}

impl SocialService {
    /// Starts the connection, event, presence and outbox tasks; they stop when `shutdown`
    /// fires. `keys` come from the keychain ([`Keys::from_secret_store`]).
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        sessions: SessionSlot,
        db: Db,
        bus: &EventBus,
        events: Arc<dyn SocialEvents>,
        idle: Arc<dyn IdleSource>,
        keys: Arc<Keys>,
        device_name: String,
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
                db: db.clone(),
                events,
                realtime,
                idle,
                friends: Mutex::new(None),
                inputs: Mutex::new(presence::Inputs {
                    show_current_game: settings.show_current_game,
                    ..presence::Inputs::default()
                }),
                sent: Mutex::new(None),
                keys,
                device_name,
                messaging: messaging::MessagingState::default(),
                games: Mutex::new(Arc::new(LocalLibrary { db: db.clone() })),
            }),
        };
        tokio::spawn(service.clone().incoming_loop(rx, shutdown.clone()));
        tokio::spawn(service.clone().connection_loop(shutdown.clone()));
        tokio::spawn(service.clone().presence_loop(
            bus.subscribe(),
            timing.idle_poll,
            shutdown.clone(),
        ));
        tokio::spawn(service.clone().outbox_loop(shutdown.clone()));
        tokio::spawn(
            service
                .clone()
                .invite_install_loop(bus.subscribe(), shutdown.clone()),
        );
        tokio::spawn(service.clone().network_loop(timing.network_poll, shutdown));
        Ok(service)
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
        Ok(SocialApi {
            http: &self.inner.http,
            session,
            sessions: &self.inner.sessions,
        })
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
        self.inner
            .db
            .call(move |c| {
                c.execute(
                    "INSERT INTO social_blocks (server_id, user_id, username, blocked_at) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (server_id, user_id) DO UPDATE SET username = COALESCE(excluded.username, username)",
                    rusqlite::params![server.to_string(), user_id.to_string(), username, blocked_at],
                )?;
                Ok(())
            })
            .await
            .map_err(|e| SocialError::internal("saving a block", &e))?;
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
        self.inner
            .db
            .call(move |c| {
                c.execute(
                    "DELETE FROM social_blocks WHERE server_id = ?1 AND user_id = ?2",
                    rusqlite::params![server.to_string(), user_id.to_string()],
                )?;
                Ok(())
            })
            .await
            .map_err(|e| SocialError::internal("removing a block", &e))?;
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
            .inner
            .db
            .call(move |c| {
                let mut stmt = c.prepare(
                    "SELECT user_id, username, blocked_at FROM social_blocks WHERE server_id = ?1
                     ORDER BY blocked_at DESC, user_id",
                )?;
                let rows = stmt
                    .query_map([server.to_string()], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<String>>(1)?,
                            r.get::<_, i64>(2)?,
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(|e| SocialError::internal("reading blocks", &e))?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, username, at)| {
                Some(BlockedUser {
                    user_id: id.parse().ok()?,
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
            Incoming::Connected { hello, .. } => {
                *lock(&self.inner.sent) = None;
                self.publish_presence();
                self.resync().await;
                self.messaging_connected(&hello).await;
                self.invites_connected().await;
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
                kinds::INVITE_CREATED | kinds::INVITE_UPDATED => {
                    self.invite_event(server_id, kind == kinds::INVITE_CREATED, data)
                        .await;
                }
                kinds::INBOX_NEW | kinds::DEVICE_ADDED | kinds::DEVICE_REVOKED | kinds::TYPING => {
                    self.messaging_event(server_id, &kind, data).await;
                }
                _ => {}
            },
        }
    }
}

#[cfg(test)]
mod tests;
