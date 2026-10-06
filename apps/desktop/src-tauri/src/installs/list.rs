//! `installs_list` (INS-04): the active server's installs in the
//! `InstalledPackage` shape the library screens use.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use serde::Serialize;
use specta::Type;
use uuid::Uuid;
use vgames_core::manifest::Platform as Host;
use vgames_proto::packages::Platform as ApiPlatform;

use crate::catalog::Platform;
use crate::catalog::release::{Route, route_for};
use crate::db::download_jobs::JobKind;
use crate::db::installs::ListedInstall;
use crate::events::PackageRef;

/// Where an install is. `incomplete`: not installed and no download job
/// exists (02 §10: resume or remove). `installing` covers queued and active
/// downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum InstallState {
    Installed,
    Incomplete,
    Installing,
    Updating,
    Repairing,
    Moving,
    Uninstalling,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename = "LaunchTarget")]
pub struct InstallTarget {
    pub id: String,
    pub label: String,
    /// The manifest's `launch.default`.
    pub is_default: bool,
}

/// How the package runs on this machine (09 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CompatLayer {
    Native,
    Proton,
    Wine,
}

/// Cloud save state of an install (06). `unsupported`: no saves declared,
/// or no cloud-save client (PLAY-06 plugs one in).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CloudSaveState {
    Unsupported,
    Synced,
    Syncing,
    Pending,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct AvailableUpdate {
    pub version_label: String,
    pub sequence: u64,
    /// Bytes to download (changed files only).
    pub download_bytes: u64,
    /// The installed version was withdrawn: offered even with a lower sequence.
    pub installed_yanked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct InstalledPackage {
    pub package: PackageRef,
    pub slug: String,
    pub title: String,
    /// `vgimg:` URL of the cached cover, served by the Rust core.
    pub cover_url: Option<String>,
    pub library_id: String,
    /// Absolute install directory, for display only.
    pub install_path: String,
    pub platform: Platform,
    pub version_label: String,
    pub sequence: u64,
    pub size_bytes: u64,
    pub installed_at: Option<String>,
    pub last_played_at: Option<String>,
    pub playtime_seconds: u64,
    pub state: InstallState,
    pub update: Option<AvailableUpdate>,
    pub favorite: bool,
    pub collection_ids: Vec<String>,
    pub running: bool,
    /// Empty for incomplete installs (the manifest is not verified yet).
    pub targets: Vec<InstallTarget>,
    pub compat: CompatLayer,
    pub cloud_saves: CloudSaveState,
}

/// Cloud-save state per install (PLAY-06 registers its client).
pub trait CloudSaveProvider: Send + Sync + 'static {
    fn state(&self, package: PackageRef) -> CloudSaveState;
}

/// The default until PLAY-06: no cloud saves.
pub struct NoCloudSaves;

impl CloudSaveProvider for NoCloudSaves {
    fn state(&self, _package: PackageRef) -> CloudSaveState {
        CloudSaveState::Unsupported
    }
}

/// What `installs_list` reads besides the database rows.
pub struct ListContext<'a> {
    pub host: Option<Host>,
    pub favorites: HashSet<Uuid>,
    pub memberships: HashMap<Uuid, Vec<Uuid>>,
    /// Kind of the download job of each package that has one.
    pub jobs: HashMap<PackageRef, JobKind>,
    pub running: &'a dyn Fn(PackageRef) -> bool,
    pub cover: &'a dyn Fn(PackageRef, Uuid) -> Option<String>,
}

/// Install-side state shared by the library commands.
pub struct Installs {
    cloud: RwLock<Arc<dyn CloudSaveProvider>>,
    updates: Mutex<HashMap<PackageRef, AvailableUpdate>>,
    /// Launch targets per (install, version): manifests are read once.
    targets: Mutex<HashMap<(PackageRef, String), Vec<InstallTarget>>>,
}

impl Default for Installs {
    fn default() -> Self {
        Self {
            cloud: RwLock::new(Arc::new(NoCloudSaves)),
            updates: Mutex::new(HashMap::new()),
            targets: Mutex::new(HashMap::new()),
        }
    }
}

impl Installs {
    /// PLAY-06 registers its cloud-save client here.
    pub fn set_cloud_save_provider(&self, provider: Arc<dyn CloudSaveProvider>) {
        if let Ok(mut slot) = self.cloud.write() {
            *slot = provider;
        }
    }

    pub fn set_update(&self, package: PackageRef, update: Option<AvailableUpdate>) {
        if let Ok(mut updates) = self.updates.lock() {
            match update {
                Some(update) => updates.insert(package, update),
                None => updates.remove(&package),
            };
        }
    }

    pub fn update(&self, package: PackageRef) -> Option<AvailableUpdate> {
        self.updates.lock().ok()?.get(&package).cloned()
    }

    /// Forgets the cached launch targets of an install (moved, updated, gone).
    pub fn forget(&self, package: PackageRef) {
        if let Ok(mut targets) = self.targets.lock() {
            targets.retain(|(p, _), _| *p != package);
        }
    }

    fn targets(&self, row: &ListedInstall) -> Vec<InstallTarget> {
        let key = (row.package, row.version_id.clone());
        if let Some(found) = self.targets.lock().ok().and_then(|t| t.get(&key).cloned()) {
            return found;
        }
        let found = read_targets(&row.root);
        if let Ok(mut targets) = self.targets.lock() {
            targets.insert(key, found.clone());
        }
        found
    }

    /// Blocking (reads manifests): call from a blocking pool.
    pub fn list(
        &self,
        rows: Vec<ListedInstall>,
        context: &ListContext<'_>,
    ) -> Vec<InstalledPackage> {
        let cloud = match self.cloud.read() {
            Ok(slot) => Arc::clone(&slot),
            Err(_) => Arc::new(NoCloudSaves),
        };
        rows.into_iter()
            .filter_map(|row| {
                let platform = ApiPlatform::parse(&row.platform)?;
                let state = state_of(&row.state, context.jobs.get(&row.package).copied());
                let targets = if state == InstallState::Installed {
                    self.targets(&row)
                } else {
                    Vec::new()
                };
                let compat = match context.host.and_then(|host| route_for(&[platform], host)) {
                    Some((_, Route::Proton)) => CompatLayer::Proton,
                    Some((_, Route::Wine { .. })) => CompatLayer::Wine,
                    _ => CompatLayer::Native,
                };
                Some(InstalledPackage {
                    package: row.package,
                    slug: row.catalog.slug.clone(),
                    title: row.catalog.title.clone(),
                    cover_url: row
                        .catalog
                        .cover_asset_id
                        .and_then(|asset| (context.cover)(row.package, asset)),
                    library_id: row.library_id.to_string(),
                    install_path: row.root.to_string_lossy().into_owned(),
                    platform: platform.into(),
                    version_label: row.catalog.version_label.clone(),
                    sequence: u64::try_from(row.sequence).unwrap_or(0),
                    size_bytes: u64::try_from(row.size_bytes).unwrap_or(0),
                    installed_at: row.installed_at.map(unix_rfc3339),
                    last_played_at: row.last_played_at.map(unix_rfc3339),
                    playtime_seconds: u64::try_from(row.playtime_seconds).unwrap_or(0),
                    state,
                    update: (state == InstallState::Installed)
                        .then(|| self.update(row.package))
                        .flatten(),
                    favorite: context.favorites.contains(&row.package.package_id),
                    collection_ids: context
                        .memberships
                        .get(&row.package.package_id)
                        .map(|ids| ids.iter().map(Uuid::to_string).collect())
                        .unwrap_or_default(),
                    running: (context.running)(row.package),
                    targets,
                    compat,
                    cloud_saves: cloud.state(row.package),
                })
            })
            .collect()
    }
}

fn state_of(stored: &str, job: Option<JobKind>) -> InstallState {
    match (stored, job) {
        (_, Some(JobKind::Update)) => InstallState::Updating,
        (_, Some(JobKind::Repair)) => InstallState::Repairing,
        ("installing", Some(JobKind::Install)) => InstallState::Installing,
        ("installing" | "broken", _) => InstallState::Incomplete,
        ("updating", _) => InstallState::Updating,
        ("repairing", _) => InstallState::Repairing,
        ("moving", _) => InstallState::Moving,
        ("uninstalling", _) => InstallState::Uninstalling,
        _ => InstallState::Installed,
    }
}

/// Launch targets from the install's stored manifest (its signature is
/// checked before every launch; this is display only).
fn read_targets(root: &Path) -> Vec<InstallTarget> {
    let path = root
        .join(vgames_transfer::install::META_DIR)
        .join(vgames_transfer::install::MANIFEST_FILE);
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    let Ok(manifest) = vgames_core::manifest::parse_and_validate(&bytes) else {
        tracing::warn!(path = %path.display(), "cannot read the stored manifest");
        return Vec::new();
    };
    manifest
        .launch
        .map(|launch| {
            launch
                .targets
                .into_iter()
                .map(|t| InstallTarget {
                    is_default: t.id == launch.default,
                    id: t.id,
                    label: t.label,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn unix_rfc3339(seconds: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(seconds)
        .map(crate::catalog::rfc3339)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_follow_the_record_and_the_queue() {
        assert_eq!(state_of("installed", None), InstallState::Installed);
        assert_eq!(
            state_of("installing", Some(JobKind::Install)),
            InstallState::Installing
        );
        assert_eq!(state_of("installing", None), InstallState::Incomplete);
        assert_eq!(
            state_of("installed", Some(JobKind::Update)),
            InstallState::Updating
        );
        assert_eq!(
            state_of("installed", Some(JobKind::Repair)),
            InstallState::Repairing
        );
        assert_eq!(state_of("moving", None), InstallState::Moving);
        assert_eq!(state_of("uninstalling", None), InstallState::Uninstalling);
    }

    fn row(platform: &str, state: &str) -> ListedInstall {
        ListedInstall {
            package: PackageRef {
                server_id: Uuid::from_u128(1),
                package_id: Uuid::from_u128(2),
            },
            library_id: Uuid::from_u128(3),
            library_path: "/games".into(),
            root: "/games/garden".into(),
            version_id: "v".into(),
            sequence: 4,
            platform: platform.into(),
            state: state.into(),
            installed_at: Some(1_790_000_000),
            last_played_at: None,
            playtime_seconds: 60,
            size_bytes: 1_000,
            catalog: crate::db::installs::CatalogInfo {
                title: "Gilded Garden".into(),
                slug: "garden".into(),
                version_label: "1.4".into(),
                cover_asset_id: Some(Uuid::from_u128(9)),
            },
        }
    }

    #[test]
    fn rows_become_library_entries() {
        let installs = Installs::default();
        let package = row("windows-x86_64", "installed").package;
        installs.set_update(
            package,
            Some(AvailableUpdate {
                version_label: "1.5".into(),
                sequence: 5,
                download_bytes: 10,
                installed_yanked: false,
            }),
        );
        let running = |p: PackageRef| p == package;
        let cover = |_: PackageRef, asset: Uuid| Some(format!("vgimg://localhost/{asset}"));
        let context = ListContext {
            host: Some(Host::LinuxX86_64),
            favorites: HashSet::from([package.package_id]),
            memberships: HashMap::from([(package.package_id, vec![Uuid::from_u128(7)])]),
            jobs: HashMap::new(),
            running: &running,
            cover: &cover,
        };
        let listed = installs.list(
            vec![
                row("windows-x86_64", "installed"),
                row("not-a-platform", "installed"),
            ],
            &context,
        );
        assert_eq!(listed.len(), 1, "unknown platforms are skipped");
        let entry = &listed[0];
        assert_eq!(entry.compat, CompatLayer::Proton);
        assert_eq!(entry.state, InstallState::Installed);
        assert!(entry.favorite && entry.running);
        assert_eq!(entry.collection_ids, vec![Uuid::from_u128(7).to_string()]);
        assert_eq!(entry.update.as_ref().unwrap().sequence, 5);
        assert_eq!(entry.cloud_saves, CloudSaveState::Unsupported);
        assert_eq!(entry.installed_at.as_deref(), Some("2026-09-21T14:13:20Z"));
        assert!(entry.cover_url.is_some());

        // An incomplete install offers no update and no targets.
        let listed = installs.list(vec![row("linux-x86_64", "installing")], &context);
        assert_eq!(listed[0].state, InstallState::Incomplete);
        assert_eq!(listed[0].compat, CompatLayer::Native);
        assert!(listed[0].update.is_none() && listed[0].targets.is_empty());
    }
}
