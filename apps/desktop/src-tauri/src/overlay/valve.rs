//! Per-package overlay switches and the crash safety valve (05-social §6.3): a game that exits
//! abnormally within 60 s of launch twice in a row with the overlay on gets the overlay turned
//! off for that package, with the time it happened; turning it on again clears the valve.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::settings::Setting;
use crate::events::{GameExit, PackageRef};

/// A quick abnormal exit is one this many seconds (or fewer) after launch.
pub const QUICK_EXIT_SECS: u32 = 60;
/// Quick abnormal exits in a row that turn the overlay off.
pub const TRIP_AFTER: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageRow {
    pub server_id: Uuid,
    pub package_id: Uuid,
    #[serde(default)]
    pub title: String,
    pub enabled: bool,
    /// Unix seconds.
    #[serde(default)]
    pub disabled_by_valve_at: Option<i64>,
    #[serde(default)]
    pub quick_crashes: u32,
}

impl PackageRow {
    pub fn package(&self) -> PackageRef {
        PackageRef {
            server_id: self.server_id,
            package_id: self.package_id,
        }
    }
}

/// Stored as one settings value (a few rows per installed game).
pub struct OverlayPackages;
impl Setting for OverlayPackages {
    const KEY: &'static str = "overlay.packages";
    type Value = Vec<PackageRow>;
    fn default_value() -> Vec<PackageRow> {
        Vec::new()
    }
}

fn row(rows: &mut Vec<PackageRow>, package: PackageRef) -> &mut PackageRow {
    let at = rows.iter().position(|r| r.package() == package);
    let i = match at {
        Some(i) => i,
        None => {
            rows.push(PackageRow {
                server_id: package.server_id,
                package_id: package.package_id,
                title: String::new(),
                enabled: true,
                disabled_by_valve_at: None,
                quick_crashes: 0,
            });
            rows.len() - 1
        }
    };
    // `i` is a valid index by construction.
    #[allow(clippy::indexing_slicing)]
    &mut rows[i]
}

/// Whether the overlay may run for `package` (missing rows default to on).
pub fn is_enabled(rows: &[PackageRow], package: PackageRef) -> bool {
    rows.iter()
        .find(|r| r.package() == package)
        .is_none_or(|r| r.enabled)
}

/// Remembers the title shown in Settings.
pub fn set_title(rows: &mut Vec<PackageRow>, package: PackageRef, title: &str) {
    let t: String = title.chars().take(128).collect();
    if !t.is_empty() {
        row(rows, package).title = t;
    }
}

/// The user's switch. Turning it on clears the valve.
pub fn set_enabled(rows: &mut Vec<PackageRow>, package: PackageRef, enabled: bool) {
    let r = row(rows, package);
    r.enabled = enabled;
    if enabled {
        r.disabled_by_valve_at = None;
        r.quick_crashes = 0;
    }
}

/// A game launched with the overlay exited. Returns `true` when this exit tripped the valve.
pub fn record_exit(
    rows: &mut Vec<PackageRow>,
    package: PackageRef,
    exit: &GameExit,
    now: i64,
) -> bool {
    let abnormal =
        !exit.stopped_by_user && exit.code != Some(0) && exit.session_seconds <= QUICK_EXIT_SECS;
    let r = row(rows, package);
    if !abnormal {
        r.quick_crashes = 0;
        return false;
    }
    r.quick_crashes = r.quick_crashes.saturating_add(1);
    if r.quick_crashes >= TRIP_AFTER && r.enabled {
        r.enabled = false;
        r.disabled_by_valve_at = Some(now);
        tracing::warn!(package = %package.package_id, "the overlay was turned off for this game after repeated quick crashes");
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg() -> PackageRef {
        PackageRef {
            server_id: Uuid::from_u128(1),
            package_id: Uuid::from_u128(2),
        }
    }

    fn exit(code: Option<i32>, by_user: bool, secs: u32) -> GameExit {
        GameExit {
            code,
            stopped_by_user: by_user,
            session_seconds: secs,
        }
    }

    #[test]
    fn two_quick_crashes_in_a_row_trip_and_re_enabling_resets() {
        let mut rows = Vec::new();
        assert!(is_enabled(&rows, pkg()));
        assert!(!record_exit(
            &mut rows,
            pkg(),
            &exit(Some(-1073741819), false, 5),
            100
        ));
        assert!(is_enabled(&rows, pkg()));
        assert!(record_exit(&mut rows, pkg(), &exit(None, false, 30), 200));
        assert!(!is_enabled(&rows, pkg()));
        assert_eq!(rows[0].disabled_by_valve_at, Some(200));
        // A third crash does not trip again.
        assert!(!record_exit(
            &mut rows,
            pkg(),
            &exit(Some(1), false, 3),
            300
        ));
        assert_eq!(rows[0].disabled_by_valve_at, Some(200));

        set_enabled(&mut rows, pkg(), true);
        assert!(is_enabled(&rows, pkg()));
        assert_eq!(rows[0].disabled_by_valve_at, None);
        assert_eq!(rows[0].quick_crashes, 0);
    }

    #[test]
    fn normal_long_or_user_stopped_exits_do_not_count_and_reset_the_streak() {
        let mut rows = Vec::new();
        assert!(!record_exit(&mut rows, pkg(), &exit(Some(3), false, 10), 1));
        // A clean exit in between resets the streak.
        assert!(!record_exit(&mut rows, pkg(), &exit(Some(0), false, 10), 2));
        assert!(!record_exit(&mut rows, pkg(), &exit(Some(3), false, 10), 3));
        // Long sessions and user stops are not "quick crashes".
        assert!(!record_exit(&mut rows, pkg(), &exit(Some(3), false, 61), 4));
        assert!(!record_exit(&mut rows, pkg(), &exit(Some(3), false, 10), 5));
        assert!(!record_exit(&mut rows, pkg(), &exit(Some(3), true, 10), 6));
        assert!(is_enabled(&rows, pkg()));
        // A user turning it off is not the valve.
        set_enabled(&mut rows, pkg(), false);
        assert!(!is_enabled(&rows, pkg()));
        assert_eq!(rows[0].disabled_by_valve_at, None);
        set_title(&mut rows, pkg(), "Arena");
        assert_eq!(rows[0].title, "Arena");
    }
}
