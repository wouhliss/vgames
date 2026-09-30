//! The invite client (A4-T09, 05-social §5, 05-social-notes §2.5).
//!
//! - **Sender.** `invite_send` creates the invite; an optional join secret (checked against
//!   the grammar) is kept locally, sealed at rest, and never sent to the server. When the
//!   invite turns `ready`, the install that sent it queues `invite.join` over Olm to the
//!   invitee's devices in their direct conversation (once).
//! - **Invitee.** `invite_accept` accepts on the server, then checks the package through the
//!   [`Games`] port: current → `ready`; missing or outdated → `invite-install-requested`, so
//!   the UI opens its install or update dialog at once and installs through the normal path
//!   (signature checks included: nothing here installs anything). Install progress from the
//!   bus is reported as `installing` every ≥ 5 s or ≥ 5 %, the end as `ready` or `failed`.
//!   `invite.join` is honoured only for an invite this install accepted, that is `ready`, and
//!   only from the invite's sender: the join target is launched with the secret as a whole
//!   argument (an invalid secret was already dropped: normal launch), then `joined`.
//! - Every `hello` resyncs the list and resumes what a restart interrupted. Cancelled,
//!   declined, expired, failed and joined invites are forgotten locally.

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_proto::realtime::InviteEvent;
use vgames_proto::social::{
    InviteCreate, InviteFailure, InviteReport, InviteState, InviteStatusUpdate,
    is_valid_join_secret,
};

use super::{SocialService, lock};
use crate::db::now_unix;
use crate::events::{AppEvent, InstallOutcome, PackageRef};
use crate::social::api::SocialApi;
use crate::social::model::{Invite, InviteInstallReason, SocialError};
use crate::social::ports::{GameCheck, Games, is_outdated};
use crate::social::store::{self, LocalInvite};

/// Longest invite message (the server's limit).
const MAX_INVITE_MESSAGE: usize = 200;

/// Maps an install failure code to the reason reported to the sender.
fn failure_from(outcome: &InstallOutcome) -> Option<InviteFailure> {
    match outcome {
        InstallOutcome::Installed => None,
        InstallOutcome::Cancelled { .. } => Some(InviteFailure::CancelledByUser),
        InstallOutcome::Failed { code, .. } => {
            let code = code.to_ascii_lowercase();
            Some(if code.contains("space") || code.contains("disk_full") {
                InviteFailure::InsufficientSpace
            } else if code.contains("no_build") || code.contains("platform") {
                InviteFailure::NoBuildForPlatform
            } else {
                InviteFailure::InstallFailed
            })
        }
    }
}

impl SocialService {
    /// Replaces the library/launch port (Agent 2's implementation, or a fake in tests).
    pub fn set_games(&self, games: Arc<dyn Games>) {
        *lock(&self.inner.games) = games;
    }

    fn games(&self) -> Arc<dyn Games> {
        lock(&self.inner.games).clone()
    }

    /// The UI model of a server invite, with `has_join_secret` from the local record.
    async fn ui_invite(&self, api: &SocialApi<'_>, i: vgames_proto::social::Invite) -> Invite {
        let server = api.session.server_id;
        let id = i.id;
        let local = self
            .with_store("reading an invite", move |c, _| {
                store::local_invite(c, server, id)
            })
            .await
            .ok()
            .flatten();
        let has_secret = matches!(
            local,
            Some(LocalInvite::Sent {
                has_secret: true,
                ..
            })
        );
        Invite::from_proto(i, api.session.user_id, server, has_secret)
    }

    /// Active and recently ended invites, both directions.
    pub async fn invites_list(&self) -> Result<Vec<Invite>, SocialError> {
        let api = self.api()?;
        let list = api.invites().await?;
        let mut out = Vec::with_capacity(list.items.len());
        for i in list.items {
            out.push(self.ui_invite(&api, i).await);
        }
        Ok(out)
    }

    /// Invites a friend to play. `join_secret` ("server address / lobby code") is optional.
    pub async fn invite_send(
        &self,
        to_user_id: Uuid,
        package_id: Uuid,
        message: Option<String>,
        join_secret: Option<String>,
    ) -> Result<Invite, SocialError> {
        let message = message
            .map(|m| m.trim().to_owned())
            .filter(|m| !m.is_empty());
        if message.as_ref().is_some_and(|m| {
            m.chars().count() > MAX_INVITE_MESSAGE || m.chars().any(|c| c.is_control() && c != '\n')
        }) {
            return Err(SocialError::invalid("message", "At most 200 characters"));
        }
        let join_secret = join_secret
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        if join_secret
            .as_deref()
            .is_some_and(|s| !is_valid_join_secret(s))
        {
            return Err(SocialError::invalid(
                "join_secret",
                "Letters, digits and . _ : - [ ] only, up to 256",
            ));
        }
        let api = self.api()?;
        let server = api.session.server_id;
        let invite = api
            .invite_create(&InviteCreate {
                to_user_id,
                package_id,
                message,
            })
            .await?;
        let id = invite.id;
        let now = now_unix();
        self.with_store("saving an invite", move |c, k| {
            store::invite_sent(c, k, server, id, package_id, join_secret.as_deref(), now)
        })
        .await?;
        let ui = self.ui_invite(&api, invite).await;
        self.inner.events.invite_changed(&ui);
        Ok(ui)
    }

    /// Accepts an incoming invite, then prepares the game in the background.
    pub async fn invite_accept(&self, invite_id: Uuid) -> Result<Invite, SocialError> {
        let api = self.api()?;
        let server = api.session.server_id;
        let invite = api.invite_action(invite_id, "accept").await?;
        let package = invite.package.id;
        let now = now_unix();
        self.with_store("saving an invite", move |c, _| {
            store::invite_accepted(c, server, invite_id, package, now)
        })
        .await?;
        let ui = self.ui_invite(&api, invite.clone()).await;
        self.inner.events.invite_changed(&ui);
        let this = self.clone();
        tokio::spawn(async move {
            if let Ok(api) = this.api()
                && api.session.server_id == server
            {
                this.prepare(&api, &invite).await;
            }
        });
        Ok(ui)
    }

    pub async fn invite_decline(&self, invite_id: Uuid) -> Result<Invite, SocialError> {
        self.end(invite_id, "decline").await
    }

    pub async fn invite_cancel(&self, invite_id: Uuid) -> Result<Invite, SocialError> {
        self.end(invite_id, "cancel").await
    }

    async fn end(&self, invite_id: Uuid, action: &str) -> Result<Invite, SocialError> {
        let api = self.api()?;
        let server = api.session.server_id;
        let invite = api.invite_action(invite_id, action).await?;
        self.with_store("forgetting an invite", move |c, _| {
            store::forget_invite(c, server, invite_id)
        })
        .await?;
        let ui = self.ui_invite(&api, invite).await;
        self.inner.events.invite_changed(&ui);
        Ok(ui)
    }

    /// Reports the invitee's state; emits the updated invite.
    async fn report(
        &self,
        api: &SocialApi<'_>,
        id: Uuid,
        update: InviteStatusUpdate,
    ) -> Result<(), SocialError> {
        let invite = api.invite_status(id, &update).await?;
        let terminal = !invite.state.is_active();
        let ui = self.ui_invite(api, invite).await;
        self.inner.events.invite_changed(&ui);
        if terminal {
            let server = api.session.server_id;
            let _ = self
                .with_store("forgetting an invite", move |c, _| {
                    store::forget_invite(c, server, id)
                })
                .await;
        }
        Ok(())
    }

    /// An installed game is outdated when the server publishes a newer release for the
    /// installed platform. A failed lookup never blocks joining: the game counts as current.
    async fn has_newer_release(&self, api: &SocialApi<'_>, server: Uuid, package: Uuid) -> bool {
        let Some(installed) = self.games().installed_build(server, package).await else {
            return false;
        };
        match api.package_releases(package).await {
            Ok(releases) => is_outdated(&installed, &releases),
            Err(error) => {
                tracing::info!(%error, %package, "cannot check for a newer release");
                false
            }
        }
    }

    /// After accepting (or after a restart): is the game ready, or does it need installing?
    async fn prepare(&self, api: &SocialApi<'_>, invite: &vgames_proto::social::Invite) {
        let server = api.session.server_id;
        let package = invite.package.id;
        let mut check = self.games().check(server, package).await;
        if check == GameCheck::Current && self.has_newer_release(api, server, package).await {
            check = GameCheck::Outdated;
        }
        let (state, failure) = match check {
            GameCheck::Current => (InviteReport::Ready, None),
            GameCheck::NoBuildForPlatform => (
                InviteReport::Failed,
                Some(InviteFailure::NoBuildForPlatform),
            ),
            check @ (GameCheck::Missing | GameCheck::Outdated) => {
                let reason = if check == GameCheck::Missing {
                    InviteInstallReason::Missing
                } else {
                    InviteInstallReason::Outdated
                };
                self.inner.events.invite_install_requested(
                    invite.id,
                    PackageRef {
                        server_id: server,
                        package_id: package,
                    },
                    reason,
                );
                return;
            }
        };
        let update = InviteStatusUpdate {
            state,
            progress: None,
            failure_reason: failure,
        };
        if let Err(error) = self.report(api, invite.id, update).await {
            tracing::info!(%error, invite = %invite.id, "invite status not reported");
        }
    }

    /// Follows installs of packages this install accepted invites for.
    pub(super) async fn invite_install_loop(
        self,
        mut bus: broadcast::Receiver<AppEvent>,
        shutdown: CancellationToken,
    ) {
        loop {
            let event = tokio::select! {
                () = shutdown.cancelled() => return,
                e = bus.recv() => match e {
                    Ok(e) => e,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                },
            };
            let (package, update) = match event {
                AppEvent::InstallProgress(p) => {
                    let fraction = if p.bytes_total == 0 {
                        0.0
                    } else {
                        (p.bytes_done as f64 / p.bytes_total as f64).clamp(0.0, 1.0)
                    };
                    (
                        p.package,
                        InviteStatusUpdate {
                            state: InviteReport::Installing,
                            progress: Some(fraction),
                            failure_reason: None,
                        },
                    )
                }
                AppEvent::InstallFinished(f) => {
                    let failure = failure_from(&f.outcome);
                    (
                        f.package,
                        InviteStatusUpdate {
                            state: if failure.is_some() {
                                InviteReport::Failed
                            } else {
                                InviteReport::Ready
                            },
                            progress: None,
                            failure_reason: failure,
                        },
                    )
                }
                _ => continue,
            };
            let Ok(api) = self.api() else { continue };
            let server = api.session.server_id;
            if package.server_id != server {
                continue;
            }
            let package_id = package.package_id;
            let invites = self
                .with_store("reading invites", move |c, _| {
                    store::accepted_for_package(c, server, package_id)
                })
                .await
                .unwrap_or_default();
            for id in invites {
                if let Some(p) = update.progress {
                    let now = now_unix();
                    let due = self
                        .with_store("throttling a report", move |c, _| {
                            store::progress_due(c, server, id, p, now)
                        })
                        .await
                        .unwrap_or(false);
                    if !due {
                        continue;
                    }
                }
                match self.report(&api, id, update.clone()).await {
                    Ok(()) => {}
                    // Already reported (409): nothing to do. Gone (404): forget it.
                    Err(SocialError::Conflict { .. }) => {}
                    Err(SocialError::NotFound) => {
                        let _ = self
                            .with_store("forgetting an invite", move |c, _| {
                                store::forget_invite(c, server, id)
                            })
                            .await;
                    }
                    Err(error) => {
                        tracing::info!(%error, invite = %id, "invite status not reported")
                    }
                }
            }
        }
    }

    /// Sender: queue `invite.join` once the invitee is ready (only the install that sent it,
    /// which holds the secret).
    async fn send_join(&self, api: &SocialApi<'_>, invite: &vgames_proto::social::Invite) {
        let server = api.session.server_id;
        let id = invite.id;
        let local = self
            .with_store("reading an invite", move |c, _| {
                store::local_invite(c, server, id)
            })
            .await
            .ok()
            .flatten();
        if !matches!(
            local,
            Some(LocalInvite::Sent {
                join_sent: false,
                ..
            })
        ) {
            return;
        }
        let conversation = match self.conversation_open_direct(invite.to.id).await {
            Ok(c) => c.id,
            Err(error) => {
                tracing::info!(%error, invite = %id, "cannot open the conversation for invite.join");
                return;
            }
        };
        let now = now_unix();
        let queued = self
            .with_store("queueing invite.join", move |c, k| {
                store::queue_invite_join(c, k, server, id, conversation, now)
            })
            .await;
        match queued {
            Ok(Some(message)) => {
                tracing::info!(invite = %id, "invite.join queued");
                self.inner.events.message_received(&message);
                self.inner.messaging.outbox.notify_one();
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, invite = %id, "invite.join not queued"),
        }
    }

    /// Invitee: `invite.join` arrived from `sender`. Launches only for an invite this install
    /// accepted, that is `ready`, and that `sender` sent.
    pub(super) async fn on_invite_join(
        &self,
        server: Uuid,
        sender: Uuid,
        invite_id: Uuid,
        join_secret: Option<String>,
    ) {
        let Ok(api) = self.api() else { return };
        if api.session.server_id != server {
            return;
        }
        let local = self
            .with_store("reading an invite", move |c, _| {
                store::local_invite(c, server, invite_id)
            })
            .await
            .ok()
            .flatten();
        if local != Some(LocalInvite::Accepted) {
            tracing::debug!(invite = %invite_id, "invite.join for an invite this install did not accept");
            return;
        }
        let invite = match api.invites().await {
            Ok(list) => list.items.into_iter().find(|i| i.id == invite_id),
            Err(error) => {
                tracing::info!(%error, "cannot check the invite for invite.join");
                return;
            }
        };
        let Some(invite) = invite else { return };
        if invite.from.id != sender
            || invite.to.id != api.session.user_id
            || invite.state != InviteState::Ready
        {
            tracing::warn!(invite = %invite_id, "ignoring an invite.join that does not match the invite");
            return;
        }
        match self
            .games()
            .launch_join(server, invite.package.id, join_secret)
            .await
        {
            Ok(()) => {
                let update = InviteStatusUpdate {
                    state: InviteReport::Joined,
                    progress: None,
                    failure_reason: None,
                };
                if let Err(error) = self.report(&api, invite_id, update).await {
                    tracing::info!(%error, "joined not reported");
                }
            }
            Err(error) => tracing::warn!(%error, invite = %invite_id, "the join launch failed"),
        }
    }

    /// Realtime `invite.created` / `invite.updated`.
    pub(super) async fn invite_event(&self, server: Uuid, created: bool, data: serde_json::Value) {
        let Ok(e) = serde_json::from_value::<InviteEvent>(data) else {
            return;
        };
        let Ok(api) = self.api() else { return };
        if api.session.server_id != server {
            return;
        }
        let invite = e.invite;
        self.follow(&api, &invite).await;
        let ui = self.ui_invite(&api, invite).await;
        if created && ui.direction == crate::social::model::InviteDirection::Incoming {
            self.inner.events.invite_received(&ui);
        } else {
            self.inner.events.invite_changed(&ui);
        }
    }

    /// Acts on an invite's current state (events and resyncs).
    async fn follow(&self, api: &SocialApi<'_>, invite: &vgames_proto::social::Invite) {
        let server = api.session.server_id;
        let id = invite.id;
        let me = api.session.user_id;
        if !invite.state.is_active() {
            let _ = self
                .with_store("forgetting an invite", move |c, _| {
                    store::forget_invite(c, server, id)
                })
                .await;
            return;
        }
        if invite.from.id == me && invite.state == InviteState::Ready {
            self.send_join(api, invite).await;
        }
    }

    /// After `hello`: resync invites and resume what a restart interrupted.
    pub(super) async fn invites_connected(&self) {
        let Ok(api) = self.api() else { return };
        let server = api.session.server_id;
        let list = match api.invites().await {
            Ok(l) => l.items,
            Err(error) => {
                tracing::debug!(%error, "invite resync failed");
                return;
            }
        };
        for invite in &list {
            self.follow(&api, invite).await;
            let id = invite.id;
            let local = self
                .with_store("reading an invite", move |c, _| {
                    store::local_invite(c, server, id)
                })
                .await
                .ok()
                .flatten();
            // Accepted here, but the install check never reported (restart, crash).
            if local == Some(LocalInvite::Accepted) && invite.state == InviteState::Accepted {
                self.prepare(&api, invite).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_outcomes_map_to_invite_failures() {
        assert_eq!(failure_from(&InstallOutcome::Installed), None);
        assert_eq!(
            failure_from(&InstallOutcome::Cancelled { kept_partial: true }),
            Some(InviteFailure::CancelledByUser)
        );
        for (code, want) in [
            ("disk_full", InviteFailure::InsufficientSpace),
            ("insufficient_space", InviteFailure::InsufficientSpace),
            ("no_build_for_platform", InviteFailure::NoBuildForPlatform),
            ("signature_invalid", InviteFailure::InstallFailed),
            ("integrity", InviteFailure::InstallFailed),
        ] {
            assert_eq!(
                failure_from(&InstallOutcome::Failed {
                    code: code.into(),
                    message: String::new()
                }),
                Some(want),
                "{code}"
            );
        }
    }
}
