//! Settings → Compatibility (GAME-07, 09-compatibility §2–§5): the runtimes on
//! disk, the default runner, per-package local overrides, the runtimes' licenses
//! and Rosetta 2. D3DMetal is deferred in v1: it is never offered, never accepted
//! as an override and has no license entry.

use std::path::Path;

use serde::Serialize;
use specta::Type;
use tauri::State;
use vgames_core::manifest::Platform;
use vgames_core::runtimes::{Arch, Catalog, Os, RuntimeId as CoreRuntimeId};

use crate::compat::overrides::{
    self, CompatOverride, CompatSettingsError, GraphicsBackend, RunnerVersion, RuntimeId,
};
use crate::compat::plan::natural_cmp;
use crate::compat::runtimes::RuntimeError;
use crate::error::AppError;
use crate::events::PackageRef;
use crate::launch::orchestrate::{host_platform, runs_natively};
use crate::state::AppState;

/// License text read per runtime.
const MAX_LICENSE_BYTES: u64 = 256 * 1024;

/// Whether Rosetta 2 is there (Apple silicon only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum RosettaState {
    Installed,
    Missing,
}

/// How Windows games run on this computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompatHost {
    /// Windows: Windows games run natively.
    Native,
    Proton,
    Wine {
        apple_silicon: bool,
        /// `None` on Intel Macs, which don't need it.
        rosetta: Option<RosettaState>,
        /// Last macOS release with full Rosetta 2, from the catalog; `None` without one.
        rosetta_last_macos: Option<String>,
    },
}

/// A runtime version on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct InstalledRuntime {
    pub runtime: RuntimeId,
    pub version: String,
    pub size_bytes: u64,
    /// Installed packages that launch with it; it is kept while this is above 0.
    pub used_by: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct CompatOverview {
    pub host: CompatHost,
    pub runtimes: Vec<InstalledRuntime>,
    /// Runner versions from the catalog that run here, newest first.
    pub runners: Vec<RunnerVersion>,
    /// Backends this Mac can use; empty elsewhere. Never `d3dmetal` in v1.
    pub graphics: Vec<GraphicsBackend>,
    /// `None` = automatic (the compat profile's choice).
    pub default_runner: Option<RunnerVersion>,
}

/// An installed package that launches through Proton or Wine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct PackageCompat {
    pub package: PackageRef,
    pub title: String,
    /// `proton` or `wine` (INS's `CompatLayer`; never `native` here).
    pub layer: crate::installs::list::CompatLayer,
    /// `None` when the package uses the defaults.
    #[serde(rename = "override")]
    pub local: Option<CompatOverride>,
}

/// Which compatibility layer this host uses for Windows builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostLayer {
    Proton,
    Wine,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeRemoveError {
    #[error("not found")]
    NotFound,
    /// Installed packages still launch with it.
    #[error("still used by {used_by} package(s)")]
    InUse { used_by: u32 },
    #[error("{detail}")]
    Io { detail: String },
}

/// A runtime's license, shipped with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct RuntimeLicense {
    pub runtime: RuntimeId,
    pub name: String,
    /// SPDX expression from the catalog.
    pub spdx: String,
    /// Plain text, rendered as text.
    pub text: String,
}

fn layer_on(host: Option<Platform>) -> Option<HostLayer> {
    match host {
        Some(Platform::LinuxX86_64) => Some(HostLayer::Proton),
        Some(Platform::MacosAarch64 | Platform::MacosX86_64) => Some(HostLayer::Wine),
        _ => None,
    }
}

/// The runner runtimes this host can use.
fn runner_ids(layer: HostLayer) -> &'static [CoreRuntimeId] {
    match layer {
        HostLayer::Proton => &[CoreRuntimeId::UmuProton, CoreRuntimeId::GeProton],
        HostLayer::Wine => &[CoreRuntimeId::WineMacos],
    }
}

fn os_arch(layer: HostLayer) -> (Os, Arch) {
    match layer {
        HostLayer::Proton => (Os::Linux, Arch::X86_64),
        // WineHQ's macOS builds are x86_64 (Rosetta 2 on Apple silicon).
        HostLayer::Wine => (Os::Macos, Arch::X86_64),
    }
}

/// Runner versions from `catalog` for `layer`, newest first.
pub fn runners(catalog: Option<&Catalog>, layer: Option<HostLayer>) -> Vec<RunnerVersion> {
    let (Some(catalog), Some(layer)) = (catalog, layer) else {
        return Vec::new();
    };
    let (os, arch) = os_arch(layer);
    let launcher = semver::Version::parse(env!("CARGO_PKG_VERSION")).ok();
    let mut found: Vec<RunnerVersion> = catalog
        .runtimes
        .iter()
        .filter(|rt| runner_ids(layer).contains(&rt.id) && rt.os == os && rt.arch == arch)
        .filter(|rt| {
            launcher.as_ref().is_some_and(|have| {
                semver::Version::parse(&rt.min_launcher_version).is_ok_and(|need| *have >= need)
            })
        })
        .map(|rt| RunnerVersion {
            runtime: rt.id.into(),
            version: rt.version.clone(),
        })
        .collect();
    // Runtimes in the default preference order (09 §4), each newest first.
    let rank = |id: RuntimeId| {
        runner_ids(layer)
            .iter()
            .position(|r| RuntimeId::from(*r) == id)
            .unwrap_or(usize::MAX)
    };
    found.sort_by(|a, b| {
        rank(a.runtime)
            .cmp(&rank(b.runtime))
            .then_with(|| natural_cmp(&b.version, &a.version))
    });
    found
}

/// Backends this host can use (macOS only; D3DMetal never in v1).
pub fn graphics(catalog: Option<&Catalog>, layer: Option<HostLayer>) -> Vec<GraphicsBackend> {
    if layer != Some(HostLayer::Wine) {
        return Vec::new();
    }
    let listed = |id: CoreRuntimeId| {
        catalog.is_some_and(|c| {
            c.runtimes
                .iter()
                .any(|rt| rt.id == id && rt.os == Os::Macos)
        })
    };
    let mut out = Vec::new();
    if listed(CoreRuntimeId::Dxmt) {
        out.push(GraphicsBackend::Dxmt);
    }
    if listed(CoreRuntimeId::DxvkMacos) {
        out.push(GraphicsBackend::Dxvk);
    }
    out.push(GraphicsBackend::Wined3d);
    out
}

/// The last macOS release the newest Wine build supports (`rosetta_sunset`).
pub fn rosetta_last_macos(catalog: Option<&Catalog>) -> Option<String> {
    catalog?
        .runtimes
        .iter()
        .filter(|rt| rt.id == CoreRuntimeId::WineMacos)
        .max_by(|a, b| natural_cmp(&a.version, &b.version))
        .and_then(|rt| rt.macos_max_supported)
        .map(|v| v.to_string())
}

async fn catalog(state: &AppState) -> Option<std::sync::Arc<Catalog>> {
    state
        .runtime_catalog
        .current()
        .await
        .ok()
        .flatten()
        .map(|a| a.catalog)
}

/// Checks a runner and a backend against the catalog for this host.
pub fn check_choice(
    catalog: Option<&Catalog>,
    layer: Option<HostLayer>,
    runner: Option<&RunnerVersion>,
    backend: Option<GraphicsBackend>,
) -> Result<(), CompatSettingsError> {
    if let Some(runner) = runner
        && !runners(catalog, layer).contains(runner)
    {
        return Err(CompatSettingsError::UnknownRunner);
    }
    if let Some(backend) = backend
        && !graphics(catalog, layer).contains(&backend)
    {
        return Err(CompatSettingsError::GraphicsUnavailable { backend });
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn compat_overview(state: State<'_, AppState>) -> Result<CompatOverview, AppError> {
    let host = host_platform();
    let layer = layer_on(host);
    let catalog = catalog(&state).await;
    let host_kind = match layer {
        None => CompatHost::Native,
        Some(HostLayer::Proton) => CompatHost::Proton,
        Some(HostLayer::Wine) => {
            let apple_silicon = host == Some(Platform::MacosAarch64);
            let rosetta = if apple_silicon {
                Some(if crate::compat::rosetta::available().await {
                    RosettaState::Installed
                } else {
                    RosettaState::Missing
                })
            } else {
                None
            };
            CompatHost::Wine {
                apple_silicon,
                rosetta,
                rosetta_last_macos: rosetta_last_macos(catalog.as_deref()),
            }
        }
    };
    let runtimes = state
        .runtime_store
        .list()
        .await
        .map_err(|e| AppError::internal("list the compatibility runtimes", &e))?
        .into_iter()
        .filter_map(|rt| {
            Some(InstalledRuntime {
                runtime: RuntimeId::parse(&rt.runtime_id)?,
                version: rt.version,
                size_bytes: rt.size,
                used_by: rt.used_by,
            })
        })
        .collect();
    Ok(CompatOverview {
        host: host_kind,
        runtimes,
        runners: runners(catalog.as_deref(), layer),
        graphics: graphics(catalog.as_deref(), layer),
        default_runner: overrides::default_runner(&state.db)
            .await
            .map_err(|e| AppError::internal("read the default runner", &e))?,
    })
}

/// `None` = automatic. A runner that isn't installed yet downloads at the next launch.
#[tauri::command]
#[specta::specta]
pub async fn compat_default_set(
    state: State<'_, AppState>,
    runner: Option<RunnerVersion>,
) -> Result<(), CompatSettingsError> {
    let catalog = catalog(&state).await;
    check_choice(
        catalog.as_deref(),
        layer_on(host_platform()),
        runner.as_ref(),
        None,
    )?;
    overrides::set_default_runner(&state.db, runner).await?;
    Ok(())
}

/// Deletes an unused runtime version from disk.
#[tauri::command]
#[specta::specta]
pub async fn runtime_remove(
    state: State<'_, AppState>,
    runtime: RuntimeId,
    version: String,
) -> Result<(), RuntimeRemoveError> {
    let installed = state.runtime_store.list().await.map_err(|e| {
        tracing::error!(%e, "list runtimes");
        RuntimeRemoveError::Io {
            detail: "Cannot read the installed runtimes.".into(),
        }
    })?;
    if !installed
        .iter()
        .any(|rt| rt.runtime_id == runtime.as_str() && rt.version == version)
    {
        return Err(RuntimeRemoveError::NotFound);
    }
    match state.runtime_store.remove(runtime.into(), &version).await {
        Ok(()) => Ok(()),
        Err(RuntimeError::InUse { used_by }) => Err(RuntimeRemoveError::InUse { used_by }),
        Err(error) => {
            tracing::error!(%error, "remove a runtime");
            Err(RuntimeRemoveError::Io {
                detail: "Cannot remove the runtime. See the log for details.".into(),
            })
        }
    }
}

/// Installed packages that launch through Proton or Wine, with their overrides.
#[tauri::command]
#[specta::specta]
pub async fn compat_packages(state: State<'_, AppState>) -> Result<Vec<PackageCompat>, AppError> {
    let host = host_platform();
    let Some(layer) = layer_on(host) else {
        return Ok(Vec::new());
    };
    let read = |e: &dyn std::error::Error| AppError::internal("read the installs", e);
    let mut out = Vec::new();
    for (package, _) in crate::db::installs::roots(&state.db)
        .await
        .map_err(|e| read(&e))?
    {
        let Some(row) = crate::db::installs::row(&state.db, package)
            .await
            .map_err(|e| read(&e))?
        else {
            continue;
        };
        let Some(platform) = Platform::ALL
            .into_iter()
            .find(|p| p.as_str() == row.platform)
        else {
            continue;
        };
        if host.is_none_or(|h| runs_natively(platform, h)) || platform != Platform::WindowsX86_64 {
            continue;
        }
        let title = crate::db::installs::catalog_info(&state.db, package)
            .await
            .map_err(|e| read(&e))?
            .map(|c| c.title)
            .unwrap_or_default();
        let local = overrides::get(&state.db, package)
            .await
            .map_err(|e| read(&e))?;
        out.push(PackageCompat {
            package,
            title,
            layer: match layer {
                HostLayer::Proton => crate::installs::list::CompatLayer::Proton,
                HostLayer::Wine => crate::installs::list::CompatLayer::Wine,
            },
            local,
        });
    }
    out.sort_by_key(|a| a.title.to_lowercase());
    Ok(out)
}

#[tauri::command]
#[specta::specta]
pub async fn compat_override_set(
    state: State<'_, AppState>,
    pkg: PackageRef,
    settings: CompatOverride,
) -> Result<(), CompatSettingsError> {
    if crate::db::installs::row(&state.db, pkg).await?.is_none() {
        return Err(CompatSettingsError::NotFound);
    }
    overrides::check(&settings)?;
    let catalog = catalog(&state).await;
    check_choice(
        catalog.as_deref(),
        layer_on(host_platform()),
        settings.runner.as_ref(),
        settings.graphics,
    )?;
    overrides::set(&state.db, pkg, &settings).await?;
    Ok(())
}

/// Back to the defaults.
#[tauri::command]
#[specta::specta]
pub async fn compat_override_reset(
    state: State<'_, AppState>,
    pkg: PackageRef,
) -> Result<(), AppError> {
    overrides::reset(&state.db, pkg)
        .await
        .map_err(|e| AppError::internal("reset the compatibility override", &e))
}

/// The license texts shipped with each installed runtime (plain text).
#[tauri::command]
#[specta::specta]
pub async fn compat_licenses(state: State<'_, AppState>) -> Result<Vec<RuntimeLicense>, AppError> {
    let catalog = catalog(&state).await;
    let installed = state
        .runtime_store
        .list()
        .await
        .map_err(|e| AppError::internal("list the compatibility runtimes", &e))?;
    let mut out = Vec::new();
    for rt in installed {
        let Some(runtime) = RuntimeId::parse(&rt.runtime_id) else {
            continue;
        };
        if runtime == RuntimeId::D3dmetal {
            continue; // deferred in v1: no Apple license screen
        }
        let dir = state.runtime_store.dir(runtime.into(), &rt.version);
        let text = tokio::task::spawn_blocking(move || license_text(&dir))
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let spdx = catalog
            .as_deref()
            .and_then(|c| {
                c.runtimes
                    .iter()
                    .find(|e| e.id == CoreRuntimeId::from(runtime) && e.version == rt.version)
            })
            .map(|e| e.license.clone())
            .unwrap_or_default();
        out.push(RuntimeLicense {
            runtime,
            name: format!("{} {}", display_name(runtime), rt.version),
            spdx,
            text,
        });
    }
    Ok(out)
}

/// Installs Rosetta 2. The UI calls this only after the player confirmed.
#[tauri::command]
#[specta::specta]
pub async fn rosetta_install() -> Result<(), AppError> {
    crate::compat::rosetta::install()
        .await
        .map_err(|e| AppError::internal("install Rosetta 2", &e))
}

pub fn display_name(runtime: RuntimeId) -> &'static str {
    match runtime {
        RuntimeId::UmuProton => "UMU-Proton",
        RuntimeId::GeProton => "GE-Proton",
        RuntimeId::UmuLauncher => "umu-launcher",
        RuntimeId::WineMacos => "Wine",
        RuntimeId::D3dmetal => "D3DMetal",
        RuntimeId::Dxmt => "DXMT",
        RuntimeId::DxvkMacos => "DXVK-macOS",
        RuntimeId::Moltenvk => "MoltenVK",
    }
}

/// The first license file (LICENSE, COPYING, …) at most two levels below `dir`,
/// read as text up to [`MAX_LICENSE_BYTES`]; links are not followed.
fn license_text(dir: &Path) -> Option<String> {
    use std::io::Read as _;
    let looks_like = |name: &str| {
        let upper = name.to_ascii_uppercase();
        upper.starts_with("LICENSE") || upper.starts_with("COPYING") || upper.starts_with("LICENCE")
    };
    let mut level = vec![dir.to_owned()];
    for _ in 0..3 {
        let mut next = Vec::new();
        for d in level {
            let Ok(entries) = std::fs::read_dir(&d) else {
                continue;
            };
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(std::fs::DirEntry::file_name);
            for entry in entries {
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_file() && looks_like(&entry.file_name().to_string_lossy()) {
                    let mut text = String::new();
                    std::fs::File::open(entry.path())
                        .ok()?
                        .take(MAX_LICENSE_BYTES)
                        .read_to_string(&mut text)
                        .ok()?;
                    return Some(text);
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

#[cfg(test)]
mod tests;
