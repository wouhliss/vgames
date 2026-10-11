//! `plan_for`: the launch plan of a Windows build on Linux (Proton, 09 §2) or
//! macOS (Wine, 09 §3, GAME-06), registered as the launcher's
//! [`PlanSource`](crate::launch::orchestrate::PlanSource) (GAME-05).
//!
//! Proton: the profile that applies to the release (fetched before the launch;
//! offline, the stored one), the newest catalog entries for umu-launcher and the
//! profile's preferred Proton (UMU-Proton by default), both installed through
//! `RuntimeStore` if missing, the pre-flight checks, the prefix from
//! [`prefix_dir`](super::prefix_dir), and the profile's allowlisted winetricks
//! verbs run once per prefix. Everything the plan carries comes from verified data:
//! launcher-owned variables are set by `LaunchPlan::prepare`, never from the profile.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use vgames_core::compat::{CompatProfile, Target};
use vgames_core::manifest::Platform;
use vgames_core::runtimes::{Arch, Catalog, Os, Runtime, RuntimeId};
use vgames_core::verify::ExpectedRelease;

use super::catalog::RuntimeCatalog;
use super::preflight::{self, Probes};
use super::profiles::{ProfileSource, Profiles};
use super::runtimes::RuntimeStore;
use crate::events::PackageRef;
use crate::launch::hooks::{HookFuture, LaunchContext, PlanSource};
use crate::launch::orchestrate::LaunchError;
use crate::launch::{LaunchPlan, ProtonPlan};

/// A winetricks verb gets this long (it downloads and runs installers).
pub const WINETRICKS_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Marker in the prefix listing the verbs already installed there.
const WINETRICKS_DONE: &str = ".vgames-winetricks";
/// Default Proton preference when the profile names none (09 §4).
const DEFAULT_PROTON: [RuntimeId; 2] = [RuntimeId::UmuProton, RuntimeId::GeProton];

fn unavailable(detail: impl Into<String>) -> LaunchError {
    LaunchError::CompatUnavailable {
        detail: detail.into(),
    }
}

pub struct CompatPlanner<S, P> {
    data_dir: PathBuf,
    db: crate::db::Db,
    catalog: Arc<RuntimeCatalog>,
    store: Arc<RuntimeStore>,
    profiles: Arc<Profiles<S>>,
    probes: Arc<P>,
    host: Option<Platform>,
}

impl<S: ProfileSource, P: Probes + Send + Sync + 'static> CompatPlanner<S, P> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        data_dir: PathBuf,
        db: crate::db::Db,
        catalog: Arc<RuntimeCatalog>,
        store: Arc<RuntimeStore>,
        profiles: Arc<Profiles<S>>,
        probes: Arc<P>,
        host: Option<Platform>,
    ) -> Self {
        Self {
            data_dir,
            db,
            catalog,
            store,
            profiles,
            probes,
            host,
        }
    }

    /// The plan for `release` on this host.
    pub async fn plan_for(
        &self,
        package: PackageRef,
        release: &ExpectedRelease,
        executable: &Path,
    ) -> Result<LaunchPlan, LaunchError> {
        match (self.host, release.platform) {
            (Some(Platform::LinuxX86_64), Platform::WindowsX86_64) => {
                self.proton(package, release).await.map(LaunchPlan::Proton)
            }
            (Some(Platform::MacosAarch64 | Platform::MacosX86_64), Platform::WindowsX86_64) => self
                .wine(package, release, executable)
                .await
                .map(LaunchPlan::Wine),
            (host, build) => Err(unavailable(format!(
                "This {} build cannot run on {} yet.",
                build.as_str(),
                host.map_or("this computer", |h| h.as_str())
            ))),
        }
    }

    async fn profile(
        &self,
        package: PackageRef,
        target: Target,
        release: &ExpectedRelease,
    ) -> Option<CompatProfile> {
        if let Err(error) = self.profiles.refresh(package).await {
            tracing::info!(%error, "using the stored compat profile");
        }
        self.profiles
            .for_release(package, target, release.platform, release.sequence)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "cannot read the compat profile; using defaults");
                None
            })
    }

    async fn catalog(&self) -> Result<Arc<Catalog>, LaunchError> {
        let accepted = match self.catalog.refresh_if_due().await {
            Ok(Some(accepted)) => Some(accepted),
            Ok(None) | Err(_) => self.catalog.current().await.ok().flatten(),
        };
        accepted.map(|a| a.catalog).ok_or_else(|| {
            unavailable("The compatibility runtime list is not available yet. Connect to the internet and try again.")
        })
    }

    async fn proton(
        &self,
        package: PackageRef,
        release: &ExpectedRelease,
    ) -> Result<ProtonPlan, LaunchError> {
        let profile = self.profile(package, Target::Linux, release).await;
        let catalog = self.catalog().await?;
        let launcher_version = env!("CARGO_PKG_VERSION");
        let umu = newest(
            &catalog,
            RuntimeId::UmuLauncher,
            Os::Linux,
            Arch::X86_64,
            launcher_version,
            None,
        )
        .ok_or_else(|| unavailable("No umu-launcher is listed for this computer."))?;
        let prefer = proton_preference(profile.as_ref());
        let min_version = profile.as_ref().and_then(|p| p.runner.min_version.clone());
        let proton = prefer
            .iter()
            .find_map(|id| {
                newest(
                    &catalog,
                    *id,
                    Os::Linux,
                    Arch::X86_64,
                    launcher_version,
                    min_version.as_deref(),
                )
            })
            .ok_or_else(|| {
                unavailable("No Proton build that this game accepts is listed for this computer.")
            })?;

        let prefix = super::prefix_dir(&self.data_dir, &package);
        let (probes, check_at) = (Arc::clone(&self.probes), prefix.clone());
        tokio::task::spawn_blocking(move || {
            let exists = check_at.join("drive_c").is_dir();
            preflight::check_proton(probes.as_ref(), &check_at, exists)
        })
        .await
        .map_err(|e| unavailable(format!("The pre-launch check stopped: {e}")))?
        .map_err(|e| unavailable(e.to_string()))?;

        let umu_dir = self.ensure(umu, package).await?;
        let proton_dir = self.ensure(proton, package).await?;
        let (umu_run, proton_path) = tokio::task::spawn_blocking(move || {
            (
                find_file(&umu_dir, "umu-run", 3),
                find_parent_of(&proton_dir, "proton", 2),
            )
        })
        .await
        .map_err(|e| unavailable(format!("Cannot read the runtime: {e}")))?;
        let umu_run =
            umu_run.ok_or_else(|| unavailable("The umu-launcher runtime has no umu-run."))?;
        let proton_path =
            proton_path.ok_or_else(|| unavailable("The Proton runtime has no proton script."))?;
        tokio::fs::create_dir_all(&prefix)
            .await
            .map_err(|e| unavailable(format!("Cannot create the game's prefix: {e}")))?;

        let plan = ProtonPlan {
            umu_run,
            proton_path,
            prefix,
            umu_game_id: profile.as_ref().and_then(|p| p.runner.umu_game_id.clone()),
            store: "none".to_owned(),
            env: profile
                .as_ref()
                .map(|p| p.runner.env.clone())
                .unwrap_or_default(),
            dll_overrides: profile
                .as_ref()
                .map(CompatProfile::wine_dll_overrides)
                .unwrap_or_default(),
        };
        if let Some(profile) = &profile {
            winetricks(&plan, &profile.runner.winetricks).await?;
        }
        Ok(plan)
    }

    async fn ensure(&self, rt: &Runtime, package: PackageRef) -> Result<PathBuf, LaunchError> {
        self.store.ensure(rt, package).await.map_err(|error| {
            tracing::warn!(runtime = rt.id.as_str(), version = %rt.version, %error, "runtime unavailable");
            unavailable(format!("{} {} could not be installed: {error}", rt.id.as_str(), rt.version))
        })
    }
}

impl<S: ProfileSource, P: Probes + Send + Sync + 'static> PlanSource for CompatPlanner<S, P> {
    fn plan<'a>(
        &'a self,
        ctx: &'a LaunchContext,
        manifest: &'a vgames_core::manifest::Manifest,
    ) -> HookFuture<'a, Result<LaunchPlan, LaunchError>> {
        let release = ExpectedRelease {
            server_id: ctx.package.server_id,
            package_id: ctx.package.package_id,
            version_id: manifest.version_id,
            platform: manifest.platform,
            sequence: manifest.sequence,
        };
        let executable = launch_executable(ctx, manifest);
        Box::pin(async move {
            tokio::select! {
                // Cancellation first: a plan that is ready on its first poll must not win over
                // a launch that was already cancelled.
                biased;
                () = ctx.cancellation.cancelled() => Err(unavailable("The launch was cancelled.")),
                plan = self.plan_for(ctx.package, &release, &executable) => plan,
            }
        })
    }
}

/// The default launch target's executable under the install root (the PE scan's
/// fallback when no scan was recorded); the root itself when there is none.
fn launch_executable(ctx: &LaunchContext, manifest: &vgames_core::manifest::Manifest) -> PathBuf {
    manifest
        .launch
        .as_ref()
        .and_then(|l| l.targets.iter().find(|t| t.id == l.default))
        .map_or_else(|| ctx.root.clone(), |t| ctx.root.join(&t.executable))
}

/// The profile's Proton preference (unknown ids skipped), or the default.
fn proton_preference(profile: Option<&CompatProfile>) -> Vec<RuntimeId> {
    let named: Vec<RuntimeId> = profile
        .map(|p| {
            p.runner
                .prefer
                .iter()
                .filter_map(|id| match id.as_str() {
                    "umu-proton" => Some(RuntimeId::UmuProton),
                    "ge-proton" => Some(RuntimeId::GeProton),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    if named.is_empty() {
        DEFAULT_PROTON.to_vec()
    } else {
        named
    }
}

/// The newest entry for `id` on this OS and architecture that this launcher may
/// use and that is at least `min_version`.
pub fn newest<'a>(
    catalog: &'a Catalog,
    id: RuntimeId,
    os: Os,
    arch: Arch,
    launcher_version: &str,
    min_version: Option<&str>,
) -> Option<&'a Runtime> {
    let launcher = semver::Version::parse(launcher_version).ok();
    catalog
        .runtimes
        .iter()
        .filter(|rt| rt.id == id && rt.os == os && rt.arch == arch)
        .filter(
            |rt| match (&launcher, semver::Version::parse(&rt.min_launcher_version)) {
                (Some(have), Ok(need)) => *have >= need,
                _ => false,
            },
        )
        .filter(|rt| min_version.is_none_or(|min| natural_cmp(&rt.version, min) != Ordering::Less))
        .max_by(|a, b| natural_cmp(&a.version, &b.version))
}

/// Compares version texts with digit runs as numbers (`GE-Proton10-2` < `GE-Proton10-17`).
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn runs(s: &str) -> Vec<(bool, &str)> {
        let mut out = Vec::new();
        let mut start = 0;
        let bytes = s.as_bytes();
        for i in 1..=bytes.len() {
            let boundary = i == bytes.len()
                || bytes.get(i).map(u8::is_ascii_digit) != bytes.get(i - 1).map(u8::is_ascii_digit);
            if boundary {
                let part = s.get(start..i).unwrap_or_default();
                out.push((part.bytes().all(|b| b.is_ascii_digit()), part));
                start = i;
            }
        }
        out
    }
    let (ra, rb) = (runs(a), runs(b));
    for ((da, pa), (db, pb)) in ra.iter().zip(rb.iter()) {
        let ord = if *da && *db {
            let (ta, tb) = (pa.trim_start_matches('0'), pb.trim_start_matches('0'));
            ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb))
        } else {
            pa.cmp(pb)
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    ra.len().cmp(&rb.len())
}

/// A regular file named `name` at most `depth` levels below `root` (links ignored).
fn find_file(root: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let mut level = vec![root.to_owned()];
    for _ in 0..=depth {
        let mut next = Vec::new();
        for dir in level {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(std::fs::DirEntry::file_name);
            for entry in entries {
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_file() && entry.file_name() == name {
                    return Some(entry.path());
                }
                if kind.is_dir() {
                    next.push(entry.path());
                }
            }
        }
        level = next;
    }
    None
}

/// The directory holding a file named `name` (the Proton build's root).
fn find_parent_of(root: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    find_file(root, name, depth).and_then(|f| f.parent().map(Path::to_owned))
}

/// Runs the profile's winetricks verbs (allowlisted by `vgames_core::compat`) that
/// the prefix does not have yet, each through `umu-run winetricks <verb>`.
async fn winetricks(plan: &ProtonPlan, verbs: &[String]) -> Result<(), LaunchError> {
    let marker = plan.prefix.join(WINETRICKS_DONE);
    let done: Vec<String> = tokio::fs::read_to_string(&marker)
        .await
        .map(|t| t.lines().map(str::to_owned).collect())
        .unwrap_or_default();
    for verb in verbs.iter().filter(|v| !done.contains(v)) {
        if !vgames_core::compat::WINETRICKS_ALLOWLIST.contains(&verb.as_str()) {
            return Err(unavailable(format!(
                "The winetricks verb {verb} is not allowed."
            )));
        }
        let mut command = tokio::process::Command::new(&plan.umu_run);
        command
            .arg("winetricks")
            .arg(verb)
            .env_clear()
            .envs(std::env::vars_os().filter(|(k, _)| {
                matches!(
                    k.to_str(),
                    Some(
                        "HOME"
                            | "PATH"
                            | "USER"
                            | "LANG"
                            | "DISPLAY"
                            | "WAYLAND_DISPLAY"
                            | "XDG_RUNTIME_DIR"
                    )
                )
            }))
            .env("WINEPREFIX", &plan.prefix)
            .env("PROTONPATH", &plan.proton_path)
            .env("GAMEID", plan.umu_game_id.as_deref().unwrap_or("0"))
            .env("STORE", &plan.store)
            .kill_on_drop(true);
        let status = tokio::time::timeout(WINETRICKS_TIMEOUT, command.status())
            .await
            .map_err(|_| {
                unavailable(format!(
                    "Installing {verb} into the game's prefix took too long."
                ))
            })?
            .map_err(|e| unavailable(format!("Cannot run winetricks: {e}")))?;
        if !status.success() {
            return Err(unavailable(format!(
                "Installing {verb} into the game's prefix failed."
            )));
        }
        let mut all = tokio::fs::read_to_string(&marker).await.unwrap_or_default();
        all.push_str(verb);
        all.push('\n');
        tokio::fs::write(&marker, all)
            .await
            .map_err(|e| unavailable(format!("Cannot record winetricks progress: {e}")))?;
    }
    Ok(())
}

mod wine;

#[cfg(test)]
mod tests;

#[cfg(all(test, target_os = "linux"))]
mod proton_e2e;

#[cfg(all(test, target_os = "macos"))]
mod wine_e2e;
