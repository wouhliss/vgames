//! Presence the launcher publishes (05-social §3): `in_game` while a game runs, `away`
//! after 10 minutes without input, `online` otherwise. The package is shared only when
//! "show what I'm playing" is on and the game belongs to the active server. Sent over the
//! realtime socket as `presence.set`, on change only, and again after every `hello`.

use std::time::Duration;

use uuid::Uuid;
use vgames_proto::social::{PresenceSetStatus, PresenceUpdate};

use crate::events::PackageRef;

/// Input idle time after which the user is `away`.
pub const AWAY_AFTER: Duration = Duration::from_secs(10 * 60);
/// How often idle time is polled (only matters when no game runs).
pub const IDLE_POLL: Duration = Duration::from_secs(30);

/// What the launcher knows about the user right now.
#[derive(Clone, Debug, Default)]
pub struct Inputs {
    /// Running games, oldest first (the newest one is shown).
    pub games: Vec<PackageRef>,
    pub idle: Option<Duration>,
    pub show_current_game: bool,
}

impl Inputs {
    pub fn game_started(&mut self, package: PackageRef) {
        self.games.retain(|g| *g != package);
        self.games.push(package);
    }

    pub fn game_stopped(&mut self, package: PackageRef) {
        self.games.retain(|g| *g != package);
    }
}

/// The presence to publish on `server_id`.
pub fn desired(inputs: &Inputs, server_id: Uuid) -> PresenceUpdate {
    if let Some(game) = inputs.games.last() {
        return PresenceUpdate {
            status: PresenceSetStatus::InGame,
            package_id: (inputs.show_current_game && game.server_id == server_id)
                .then_some(game.package_id),
        };
    }
    let away = inputs.idle.is_some_and(|d| d >= AWAY_AFTER);
    PresenceUpdate {
        status: if away {
            PresenceSetStatus::Away
        } else {
            PresenceSetStatus::Online
        },
        package_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(server: u128, package: u128) -> PackageRef {
        PackageRef {
            server_id: Uuid::from_u128(server),
            package_id: Uuid::from_u128(package),
        }
    }

    #[test]
    fn games_idle_and_privacy() {
        let server = Uuid::from_u128(1);
        let mut i = Inputs {
            show_current_game: true,
            ..Inputs::default()
        };
        assert_eq!(desired(&i, server).status, PresenceSetStatus::Online);
        i.idle = Some(AWAY_AFTER - Duration::from_secs(1));
        assert_eq!(desired(&i, server).status, PresenceSetStatus::Online);
        i.idle = Some(AWAY_AFTER);
        assert_eq!(desired(&i, server).status, PresenceSetStatus::Away);

        // A running game wins over idle time; its package is shared for this server only.
        i.game_started(pkg(1, 10));
        assert_eq!(
            desired(&i, server),
            PresenceUpdate {
                status: PresenceSetStatus::InGame,
                package_id: Some(Uuid::from_u128(10))
            }
        );
        i.game_started(pkg(2, 20));
        assert_eq!(
            desired(&i, server).package_id,
            None,
            "a game from another server is not named"
        );
        i.game_stopped(pkg(2, 20));
        assert_eq!(desired(&i, server).package_id, Some(Uuid::from_u128(10)));
        i.show_current_game = false;
        assert_eq!(
            desired(&i, server),
            PresenceUpdate {
                status: PresenceSetStatus::InGame,
                package_id: None
            }
        );
        i.game_stopped(pkg(1, 10));
        assert_eq!(desired(&i, server).status, PresenceSetStatus::Away);
    }
}
