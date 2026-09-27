//! Filesystem boundary for launcher libraries. Database registration and UI
//! commands call this module from a blocking pool.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use vgames_transfer::{fsutil, sys};

const MARKER: &str = ".vgames-library.json";
const MARKER_FORMAT: &str = "vgames.library/1";
const MAX_MARKER_BYTES: u64 = 4096;

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("library path is a system folder or home folder")]
    SystemFolder,
    #[error("library path overlaps an existing library")]
    Overlap,
    #[error("library path is not a directory")]
    NotDirectory,
    #[error("library already has a marker")]
    AlreadyRegistered,
    #[error("library marker is invalid or belongs to another library")]
    InvalidMarker,
    #[error("cannot {action} {path}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl LibraryError {
    fn io(action: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.to_owned(),
            source,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryRoot {
    pub id: Uuid,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryPresence {
    Online { free_bytes: u64 },
    Offline,
    MarkerChanged,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    format: String,
    library_id: Uuid,
}

/// Canonicalizes an existing folder, rejects unsafe placement and overlaps,
/// and confirms it is writable using a private create-and-delete probe.
pub fn validate_new_root(path: &Path, existing: &[PathBuf]) -> Result<PathBuf, LibraryError> {
    let root = fs::canonicalize(path).map_err(|error| LibraryError::io("open", path, error))?;
    if !fs::metadata(&root)
        .map_err(|error| LibraryError::io("inspect", &root, error))?
        .is_dir()
    {
        return Err(LibraryError::NotDirectory);
    }
    if is_system_folder(&root) {
        return Err(LibraryError::SystemFolder);
    }
    if existing
        .iter()
        .any(|other| root.starts_with(other) || other.starts_with(&root))
    {
        return Err(LibraryError::Overlap);
    }
    let probe = root.join(format!(".vgames-probe-{}", Uuid::now_v7()));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|error| LibraryError::io("test writing to", &root, error))?;
    if let Err(error) = file.sync_all() {
        let _ = fs::remove_file(&probe);
        return Err(LibraryError::io("flush", &probe, error));
    }
    fs::remove_file(&probe).map_err(|error| LibraryError::io("remove", &probe, error))?;
    Ok(root)
}

/// Writes the marker only after validation. The database caller must persist
/// this ID and canonical path in one transaction; an orphan marker can be
/// recovered or removed without deleting player files.
pub fn create_root(path: &Path, existing: &[PathBuf]) -> Result<LibraryRoot, LibraryError> {
    let path = validate_new_root(path, existing)?;
    let id = Uuid::now_v7();
    let marker_path = path.join(MARKER);
    let marker = Marker {
        format: MARKER_FORMAT.to_owned(),
        library_id: id,
    };
    let bytes = serde_json::to_vec(&marker).map_err(|_| LibraryError::InvalidMarker)?;
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker_path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return Err(LibraryError::AlreadyRegistered);
        }
        Err(error) => return Err(LibraryError::io("create", &marker_path, error)),
    };
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&marker_path);
        return Err(LibraryError::io("write", &marker_path, error));
    }
    drop(file);
    if let Err(error) = fsutil::sync_dir(&path) {
        let _ = fs::remove_file(&marker_path);
        return Err(LibraryError::io("flush", &path, error));
    }
    Ok(LibraryRoot { id, path })
}

/// Undo a marker we just created if database registration fails. Refuses to
/// remove a marker whose identity changed in the meantime.
pub fn remove_marker_if_matches(root: &LibraryRoot) -> Result<(), LibraryError> {
    let metadata = fs::symlink_metadata(&root.path)
        .map_err(|error| LibraryError::io("inspect", &root.path, error))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || fs::canonicalize(&root.path)
            .map_err(|error| LibraryError::io("open", &root.path, error))?
            != root.path
    {
        return Err(LibraryError::InvalidMarker);
    }
    if read_marker(&root.path)? != Some(root.id) {
        return Err(LibraryError::InvalidMarker);
    }
    let path = root.path.join(MARKER);
    fs::remove_file(&path).map_err(|error| LibraryError::io("remove", &path, error))?;
    fsutil::sync_dir(&root.path).map_err(|error| LibraryError::io("flush", &root.path, error))
}

/// Checks a stored library at startup. A missing drive is shown offline;
/// a changed marker is blocked so installs cannot be read from another drive.
pub fn inspect_root(root: &LibraryRoot) -> Result<LibraryPresence, LibraryError> {
    let canonical = match fs::canonicalize(&root.path) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(LibraryPresence::Offline);
        }
        Err(error) => return Err(LibraryError::io("open", &root.path, error)),
    };
    if canonical != root.path
        || !fs::metadata(&canonical)
            .map_err(|error| LibraryError::io("inspect", &canonical, error))?
            .is_dir()
        || read_marker(&canonical)? != Some(root.id)
    {
        return Ok(LibraryPresence::MarkerChanged);
    }
    let free_bytes = sys::available_space(&canonical)
        .map_err(|error| LibraryError::io("inspect free space of", &canonical, error))?;
    Ok(LibraryPresence::Online { free_bytes })
}

fn read_marker(root: &Path) -> Result<Option<Uuid>, LibraryError> {
    let path = root.join(MARKER);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(LibraryError::io("inspect", &path, error)),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_MARKER_BYTES
    {
        return Err(LibraryError::InvalidMarker);
    }
    let file =
        fsutil::open_for_read(&path).map_err(|error| LibraryError::io("open", &path, error))?;
    let mut bytes = Vec::new();
    file.take(MAX_MARKER_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| LibraryError::io("read", &path, error))?;
    if bytes.len() as u64 > MAX_MARKER_BYTES {
        return Err(LibraryError::InvalidMarker);
    }
    let marker: Marker = serde_json::from_slice(&bytes).map_err(|_| LibraryError::InvalidMarker)?;
    if marker.format != MARKER_FORMAT {
        return Err(LibraryError::InvalidMarker);
    }
    Ok(Some(marker.library_id))
}

fn is_system_folder(path: &Path) -> bool {
    if path.parent().is_none() {
        return true;
    }
    if directories::BaseDirs::new().is_some_and(|dirs| path == dirs.home_dir()) {
        return true;
    }
    #[cfg(unix)]
    for prefix in [
        "/bin", "/boot", "/dev", "/etc", "/lib", "/lib64", "/opt", "/proc", "/run", "/sbin",
        "/sys", "/usr", "/var",
    ] {
        if path.starts_with(prefix) {
            return true;
        }
    }
    #[cfg(target_os = "macos")]
    for prefix in ["/Applications", "/Library", "/System", "/private"] {
        if path.starts_with(prefix) {
            return true;
        }
    }
    #[cfg(windows)]
    for name in ["WINDIR", "ProgramFiles", "ProgramFiles(x86)"] {
        if std::env::var_os(name).is_some_and(|value| path.starts_with(Path::new(&value))) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree_tempdir() -> tempfile::TempDir {
        tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap()
    }

    #[test]
    fn rejects_nested_and_parent_libraries() {
        let dir = worktree_tempdir();
        let first = dir.path().join("first");
        let child = first.join("child");
        fs::create_dir_all(&child).unwrap();
        let first = fs::canonicalize(first).unwrap();
        assert!(matches!(
            validate_new_root(&child, std::slice::from_ref(&first)),
            Err(LibraryError::Overlap)
        ));
        assert!(matches!(
            validate_new_root(dir.path(), &[first]),
            Err(LibraryError::Overlap)
        ));
    }

    #[test]
    fn marker_identifies_a_library_and_detects_missing_or_changed_drives() {
        let dir = worktree_tempdir();
        let library = create_root(dir.path(), &[]).unwrap();
        assert!(matches!(
            inspect_root(&library).unwrap(),
            LibraryPresence::Online { .. }
        ));
        assert!(matches!(
            create_root(dir.path(), &[]),
            Err(LibraryError::AlreadyRegistered)
        ));
        fs::write(library.path.join(MARKER), b"wrong").unwrap();
        assert!(matches!(
            inspect_root(&library),
            Err(LibraryError::InvalidMarker)
        ));
        fs::remove_file(library.path.join(MARKER)).unwrap();
        assert_eq!(
            inspect_root(&library).unwrap(),
            LibraryPresence::MarkerChanged
        );
        let missing = LibraryRoot {
            path: dir.path().join("gone"),
            ..library
        };
        assert_eq!(inspect_root(&missing).unwrap(), LibraryPresence::Offline);
    }

    #[test]
    fn a_file_replacing_a_library_is_not_treated_as_online() {
        let dir = worktree_tempdir();
        let path = dir.path().join("library");
        fs::create_dir(&path).unwrap();
        let library = create_root(&path, &[]).unwrap();
        fs::remove_file(path.join(MARKER)).unwrap();
        fs::remove_dir(&path).unwrap();
        fs::write(&path, b"not a library").unwrap();
        assert_eq!(
            inspect_root(&library).unwrap(),
            LibraryPresence::MarkerChanged
        );
    }

    #[cfg(unix)]
    #[test]
    fn marker_cleanup_does_not_follow_a_replaced_library_link() {
        use std::os::unix::fs::symlink;
        let dir = worktree_tempdir();
        let path = dir.path().join("library");
        let moved = dir.path().join("moved");
        fs::create_dir(&path).unwrap();
        let library = create_root(&path, &[]).unwrap();
        fs::rename(&path, &moved).unwrap();
        symlink(&moved, &path).unwrap();
        assert!(matches!(
            remove_marker_if_matches(&library),
            Err(LibraryError::InvalidMarker)
        ));
        assert!(moved.join(MARKER).exists());
    }

    #[test]
    fn refuses_files_and_system_root() {
        let dir = worktree_tempdir();
        let file = dir.path().join("file");
        fs::write(&file, b"x").unwrap();
        assert!(matches!(
            validate_new_root(&file, &[]),
            Err(LibraryError::NotDirectory)
        ));
        #[cfg(unix)]
        assert!(matches!(
            validate_new_root(Path::new("/"), &[]),
            Err(LibraryError::SystemFolder)
        ));
    }
}
