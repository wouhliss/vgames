//! `plan_for` on macOS: Wine with a Metal graphics backend (09-compatibility §3 v1.1,
//! GAME-06). D3DMetal is deferred: a profile listing `d3dmetal` is honoured from its
//! next backend on, and a D3D12 title is refused up front.

use std::path::{Path, PathBuf};
use std::time::Duration;

use vgames_core::compat::{CompatProfile, Graphics, Target};
use vgames_core::runtimes::{Arch, Os, RuntimeId};
use vgames_core::verify::ExpectedRelease;

use super::{CompatPlanner, find_file, newest, unavailable};
use crate::compat::d3d;
use crate::compat::pe::D3dImports;
use crate::compat::preflight::Probes;
use crate::compat::profiles::ProfileSource;
use crate::events::PackageRef;
use crate::launch::WinePlan;
use crate::launch::orchestrate::LaunchError;

/// The default chain on every Mac (09 §3).
pub const DEFAULT_GRAPHICS: [Graphics; 3] = [Graphics::Dxmt, Graphics::Dxvk, Graphics::Wined3d];
/// Creating a new prefix (`wineboot --init`) gets this long.
pub const WINEBOOT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

pub const D3D12_ON_MAC: &str = "This game needs DirectX 12, which is not available on Mac yet.";
pub const NEEDS_ROSETTA: &str = "This game runs through Wine, which needs Rosetta 2. Install Rosetta from the game's page and try again.";

/// The backend chain to try: the profile's (without `d3dmetal`) or the default.
pub fn graphics_chain(profile: Option<&CompatProfile>) -> Vec<Graphics> {
    let listed: Vec<Graphics> = profile
        .map(|p| {
            p.runner
                .graphics
                .iter()
                .copied()
                .filter(|g| *g != Graphics::D3dmetal)
                .collect()
        })
        .unwrap_or_default();
    if listed.is_empty() {
        DEFAULT_GRAPHICS.to_vec()
    } else {
        listed
    }
}

/// Whether `backend` can draw what `imports` needs (DXMT is D3D10/11 only).
pub fn serves(backend: Graphics, imports: &D3dImports) -> bool {
    match backend {
        Graphics::Dxmt => !imports.d3d9 || imports.d3d10 || imports.d3d11,
        Graphics::Dxvk | Graphics::Wined3d => true,
        Graphics::D3dmetal => false,
    }
}

/// `WINEDLLOVERRIDES` entries a backend needs (`name=mode;…`).
pub fn backend_overrides(backend: Graphics) -> &'static str {
    match backend {
        Graphics::Dxmt => "d3d11,d3d10core,dxgi=n,b",
        Graphics::Dxvk => "d3d9,d3d11,d3d10core,dxgi=n,b",
        Graphics::Wined3d | Graphics::D3dmetal => "",
    }
}

fn runtime_of(backend: Graphics) -> Option<RuntimeId> {
    match backend {
        Graphics::Dxmt => Some(RuntimeId::Dxmt),
        Graphics::Dxvk => Some(RuntimeId::DxvkMacos),
        Graphics::Wined3d | Graphics::D3dmetal => None,
    }
}

impl<S: ProfileSource, P: Probes + Send + Sync + 'static> CompatPlanner<S, P> {
    pub(super) async fn wine(
        &self,
        package: PackageRef,
        release: &ExpectedRelease,
        executable: &Path,
    ) -> Result<WinePlan, LaunchError> {
        let probes = std::sync::Arc::clone(&self.probes);
        let x86_runs = tokio::task::spawn_blocking(move || probes.x86_64_runs())
            .await
            .unwrap_or(false);
        if !x86_runs {
            return Err(unavailable(NEEDS_ROSETTA));
        }
        let imports = match d3d::recorded(&self.db, package, release.version_id).await {
            Ok(Some(found)) => found,
            _ => d3d::scan_and_record(&self.db, package, release.version_id, executable)
                .await
                .unwrap_or_else(|error| {
                    tracing::info!(%error, "launch executable not scanned; assuming D3D11");
                    D3dImports {
                        d3d11: true,
                        ..D3dImports::default()
                    }
                }),
        };
        if imports.d3d12 {
            return Err(unavailable(D3D12_ON_MAC));
        }

        let profile = self.profile(package, Target::Macos, release).await;
        let catalog = self.catalog().await?;
        let version = env!("CARGO_PKG_VERSION");
        // WineHQ's macOS builds are x86_64 (Rosetta 2 on Apple silicon).
        let wine_rt = newest(
            &catalog,
            RuntimeId::WineMacos,
            Os::Macos,
            Arch::X86_64,
            version,
            None,
        )
        .ok_or_else(|| unavailable("No Wine build is listed for this Mac."))?;
        let chain = graphics_chain(profile.as_ref());
        let (graphics, backend_rt) = chain
            .iter()
            .filter(|g| serves(**g, &imports))
            .find_map(|g| match runtime_of(*g) {
                None => Some((*g, None)),
                Some(id) => newest(&catalog, id, Os::Macos, Arch::X86_64, version, None)
                    .or_else(|| newest(&catalog, id, Os::Macos, Arch::Aarch64, version, None))
                    .map(|rt| (*g, Some(rt))),
            })
            .ok_or_else(|| {
                unavailable("No graphics backend this game accepts is available for this Mac.")
            })?;

        let wine_dir = self.ensure(wine_rt, package).await?;
        let backend_dir = match backend_rt {
            Some(rt) => Some(self.ensure(rt, package).await?),
            None => None,
        };
        let wine = {
            let dir = wine_dir.clone();
            tokio::task::spawn_blocking(move || {
                find_file(&dir, "wine", 6).or_else(|| find_file(&dir, "wine64", 6))
            })
            .await
            .ok()
            .flatten()
            .ok_or_else(|| unavailable("The Wine runtime has no wine binary."))?
        };
        let prefix = crate::compat::prefix_dir(&self.data_dir, &package);
        prepare_prefix(&wine, &prefix).await?;
        if let Some(dir) = backend_dir {
            let (wine, prefix) = (wine.clone(), prefix.clone());
            tokio::task::spawn_blocking(move || install_backend(graphics, &dir, &wine, &prefix))
                .await
                .map_err(|e| unavailable(format!("Cannot install the graphics backend: {e}")))?
                .map_err(|e| unavailable(format!("Cannot install the graphics backend: {e}")))?;
        }

        let mut overrides = backend_overrides(graphics).to_owned();
        if let Some(extra) = profile.as_ref().map(CompatProfile::wine_dll_overrides)
            && !extra.is_empty()
        {
            if !overrides.is_empty() {
                overrides.push(';');
            }
            overrides.push_str(&extra);
        }
        let env = profile
            .as_ref()
            .map(|p| p.runner.env.clone())
            .unwrap_or_default();
        Ok(WinePlan {
            wine,
            prefix,
            graphics,
            env,
            dll_overrides: overrides,
        })
    }
}

/// Creates the prefix on first use (`wine wineboot --init`), with the Retina default
/// (`HKCU\\Software\\Wine\\Mac Driver`, `RetinaMode=y`).
async fn prepare_prefix(wine: &Path, prefix: &Path) -> Result<(), LaunchError> {
    if prefix.join("drive_c").is_dir() {
        return Ok(());
    }
    tokio::fs::create_dir_all(prefix)
        .await
        .map_err(|e| unavailable(format!("Cannot create the game's prefix: {e}")))?;
    run_wine(wine, prefix, &["wineboot", "--init"]).await?;
    run_wine(
        wine,
        prefix,
        &[
            "reg",
            "add",
            r"HKCU\Software\Wine\Mac Driver",
            "/v",
            "RetinaMode",
            "/t",
            "REG_SZ",
            "/d",
            "y",
            "/f",
        ],
    )
    .await
}

async fn run_wine(wine: &Path, prefix: &Path, args: &[&str]) -> Result<(), LaunchError> {
    let mut command = tokio::process::Command::new(wine);
    command
        .args(args)
        .env_clear()
        .envs(std::env::vars_os().filter(|(k, _)| {
            matches!(
                k.to_str(),
                Some("HOME" | "PATH" | "USER" | "LANG" | "TMPDIR")
            )
        }))
        .env("WINEPREFIX", prefix)
        .env("WINEDEBUG", "-all")
        .kill_on_drop(true);
    let status = tokio::time::timeout(WINEBOOT_TIMEOUT, command.status())
        .await
        .map_err(|_| unavailable("Creating the game's Wine prefix took too long."))?
        .map_err(|e| unavailable(format!("Cannot run Wine: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(unavailable("Creating the game's Wine prefix failed."))
    }
}

/// Puts a backend's Windows DLLs into the prefix (`system32`) and, for DXMT, its
/// Unix bridge next to Wine's own libraries. Only files from the verified runtime
/// are copied; nothing outside the prefix and the runtime directories is written.
fn install_backend(
    backend: Graphics,
    runtime: &Path,
    wine: &Path,
    prefix: &Path,
) -> std::io::Result<()> {
    let system32 = prefix.join("drive_c/windows/system32");
    std::fs::create_dir_all(&system32)?;
    let windows_dlls = ["x64", "x86_64-windows"]
        .iter()
        .find_map(|name| find_dir(runtime, name, 3));
    if let Some(dir) = windows_dlls {
        for entry in std::fs::read_dir(dir)?.flatten() {
            let path = entry.path();
            if entry.file_type()?.is_file() && path.extension().is_some_and(|e| e == "dll") {
                std::fs::copy(&path, system32.join(entry.file_name()))?;
            }
        }
    }
    if backend == Graphics::Dxmt
        && let (Some(unix), Some(wine_lib)) = (
            find_dir(runtime, "x86_64-unix", 3),
            wine.parent()
                .and_then(Path::parent)
                .map(|root| root.join("lib/wine/x86_64-unix")),
        )
        && wine_lib.is_dir()
    {
        for entry in std::fs::read_dir(unix)?.flatten() {
            if entry.file_type()?.is_file() {
                std::fs::copy(entry.path(), wine_lib.join(entry.file_name()))?;
            }
        }
    }
    Ok(())
}

/// A directory named `name` at most `depth` levels below `root` (links ignored).
fn find_dir(root: &Path, name: &str, depth: usize) -> Option<PathBuf> {
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
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    if entry.file_name() == name {
                        return Some(entry.path());
                    }
                    next.push(entry.path());
                }
            }
        }
        level = next;
    }
    None
}
