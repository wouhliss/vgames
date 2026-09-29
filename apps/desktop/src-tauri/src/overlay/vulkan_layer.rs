//! Vulkan implicit layer registration (05-social §6.1, A4-T11).
//!
//! The in-game renderer ships as a library next to the launcher executable. On Linux the
//! launcher keeps a per-user implicit layer manifest pointing at it in
//! `$XDG_DATA_HOME/vulkan/implicit_layer.d/`. The layer only loads into a process whose
//! environment has `VGAMES_OVERLAY=1`, which the launcher sets for games started with the
//! overlay, so every other Vulkan app on the machine is untouched. Without the library the
//! manifest is removed rather than left pointing at nothing.
//!
//! Windows registers the same manifest under `HKCU\Software\Khronos\Vulkan\ImplicitLayers`;
//! that part lands with the Windows renderer build (A4-T11).

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::json;
use vgames_overlay::protocol::env;

pub const LAYER_NAME: &str = "VK_LAYER_VGAMES_overlay";
pub const MANIFEST_FILE: &str = "vgames_overlay_layer.json";
/// Set to `1` to keep the layer out of a process that has `VGAMES_OVERLAY=1`.
pub const DISABLE_ENV: &str = "VGAMES_OVERLAY_DISABLE";

/// The renderer library's file name on this OS.
pub const LIBRARY_FILE: &str = if cfg!(windows) {
    "vgames_overlay.dll"
} else if cfg!(target_os = "macos") {
    "libvgames_overlay.dylib"
} else {
    "libvgames_overlay.so"
};

/// What [`sync`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Registration {
    Written,
    Unchanged,
    Removed,
    /// No library and no manifest.
    Absent,
}

/// The manifest for `library` (an absolute path).
pub fn manifest(library: &Path) -> Option<String> {
    if !library.is_absolute() {
        return None;
    }
    let value = json!({
        "file_format_version": "1.0.0",
        "layer": {
            "name": LAYER_NAME,
            "type": "GLOBAL",
            "library_path": library.to_str()?,
            "api_version": "1.3.0",
            "implementation_version": "1",
            "description": "vgames in-game overlay",
            "enable_environment": { env::ENABLED: "1" },
            "disable_environment": { DISABLE_ENV: "1" },
        }
    });
    serde_json::to_string_pretty(&value).ok()
}

/// `$XDG_DATA_HOME/vulkan/implicit_layer.d`, else `~/.local/share/vulkan/implicit_layer.d`.
pub fn manifest_dir(xdg_data_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = xdg_data_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|h| h.join(".local").join("share"))
        })?;
    Some(base.join("vulkan").join("implicit_layer.d"))
}

/// Writes, keeps or removes the manifest in `dir` so that it matches `library`
/// (`None` or a missing file: no layer).
pub fn sync(dir: &Path, library: Option<&Path>) -> io::Result<Registration> {
    let path = dir.join(MANIFEST_FILE);
    let wanted = library.filter(|l| l.is_file()).and_then(manifest);
    let Some(wanted) = wanted else {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(Registration::Removed),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Registration::Absent),
            Err(e) => Err(e),
        };
    };
    if std::fs::read_to_string(&path).is_ok_and(|current| current == wanted) {
        return Ok(Registration::Unchanged);
    }
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{MANIFEST_FILE}.tmp"));
    std::fs::write(&tmp, wanted.as_bytes())?;
    std::fs::rename(&tmp, &path)?;
    Ok(Registration::Written)
}

/// Registers the layer for this install (Linux; a no-op elsewhere for now). Never fails the
/// launcher: problems are logged and games run without the in-game renderer.
pub fn register_for_this_install() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let Some(dir) = manifest_dir(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
    else {
        tracing::debug!("no data directory for the Vulkan layer manifest");
        return;
    };
    let library = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(LIBRARY_FILE)));
    match sync(&dir, library.as_deref()) {
        Ok(result) => tracing::debug!(?result, "Vulkan overlay layer"),
        Err(error) => tracing::warn!(%error, "cannot register the Vulkan overlay layer"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn the_manifest_loads_only_with_the_overlay_variable() {
        let lib = std::env::temp_dir()
            .join("a \"quoted\" dir")
            .join(LIBRARY_FILE);
        let text = manifest(&lib).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let layer = &v["layer"];
        assert_eq!(layer["name"], LAYER_NAME);
        assert_eq!(layer["library_path"], lib.to_str().unwrap());
        assert_eq!(layer["enable_environment"]["VGAMES_OVERLAY"], "1");
        assert_eq!(layer["disable_environment"][DISABLE_ENV], "1");
        assert!(manifest(Path::new("relative/lib.so")).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn the_manifest_directory_follows_xdg() {
        let abs = |p: &str| Some(OsString::from(p));
        assert_eq!(
            manifest_dir(abs("/data"), abs("/home/p")).unwrap(),
            Path::new("/data/vulkan/implicit_layer.d")
        );
        assert_eq!(
            manifest_dir(abs("relative"), abs("/home/p")).unwrap(),
            Path::new("/home/p/.local/share/vulkan/implicit_layer.d")
        );
        assert_eq!(manifest_dir(None, abs("relative")), None);
        assert_eq!(manifest_dir(None, None), None);
    }

    #[test]
    fn sync_writes_keeps_and_removes_the_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vulkan").join("implicit_layer.d");
        let lib = tmp.path().join(LIBRARY_FILE);

        // No library shipped: nothing registered.
        assert_eq!(sync(&dir, Some(&lib)).unwrap(), Registration::Absent);
        assert_eq!(sync(&dir, None).unwrap(), Registration::Absent);

        std::fs::write(&lib, b"\x7fELF").unwrap();
        assert_eq!(sync(&dir, Some(&lib)).unwrap(), Registration::Written);
        assert_eq!(sync(&dir, Some(&lib)).unwrap(), Registration::Unchanged);
        let written = std::fs::read_to_string(dir.join(MANIFEST_FILE)).unwrap();
        assert_eq!(Some(written), manifest(&lib));
        assert!(!dir.join(format!("{MANIFEST_FILE}.tmp")).exists());

        // The launcher moved: the manifest follows.
        let moved = tmp.path().join("moved");
        std::fs::create_dir_all(&moved).unwrap();
        let lib2 = moved.join(LIBRARY_FILE);
        std::fs::write(&lib2, b"\x7fELF").unwrap();
        assert_eq!(sync(&dir, Some(&lib2)).unwrap(), Registration::Written);

        // The library is gone: the manifest goes too.
        std::fs::remove_file(&lib2).unwrap();
        assert_eq!(sync(&dir, Some(&lib2)).unwrap(), Registration::Removed);
        assert!(!dir.join(MANIFEST_FILE).exists());
    }
}
