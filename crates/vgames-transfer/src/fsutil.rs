//! Filesystem helpers shared by the engines: atomic writes and an install
//! root that never follows symlinks or junctions (02-package-format §3: the
//! launcher re-checks every target path, defense in depth against links
//! planted in the install directory).

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Why a target path was refused.
#[derive(Debug, thiserror::Error)]
pub enum SafePathError {
    #[error("{0} is a link; install folders never contain links")]
    Link(PathBuf),
    #[error("{0} is in the way (not a folder)")]
    NotADirectory(PathBuf),
    #[error("{0} is in the way (not a regular file)")]
    NotAFile(PathBuf),
    #[error("{0} is outside the install folder")]
    Escapes(PathBuf),
    #[error("cannot use {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl SafePathError {
    fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_owned(),
            source,
        }
    }

    /// The underlying OS error, if any.
    pub fn io_error(&self) -> Option<&io::Error> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Writes `bytes` to `path` atomically: temp file, fsync, rename, fsync the
/// directory. Readers see the old or the new content, never a mix.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let (tmp, mut file) = loop {
        let mut name = path.file_name().unwrap_or_default().to_owned();
        name.push(format!(".{}.tmp", uuid::Uuid::now_v7()));
        let tmp = path.with_file_name(name);
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(file) => break (PendingWrite(tmp), file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    {
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    drop(file);
    fs::rename(&tmp.0, path)?;
    if let Some(dir) = path.parent() {
        sync_dir(dir)?;
    }
    Ok(())
}

/// Remove only the temporary file this write created, including on error.
struct PendingWrite(PathBuf);

impl Drop for PendingWrite {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Makes a rename or create in `dir` durable (no-op where the OS cannot).
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Opens an existing regular file for positional writes without following a
/// final symlink.
pub fn open_for_write(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

/// Positional write of the whole buffer (`pwrite` / `seek_write`).
pub fn write_all_at(file: &File, mut data: &[u8], mut offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        let _ = &mut data;
        let _ = &mut offset;
        file.write_all_at(data, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        while !data.is_empty() {
            let n = file.seek_write(data, offset)?;
            if n == 0 {
                return Err(io::Error::from(io::ErrorKind::WriteZero));
            }
            data = data.get(n..).unwrap_or_default();
            offset += n as u64;
        }
        Ok(())
    }
}

/// Positional read of exactly `buf.len()` bytes.
pub fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0usize;
        while done < buf.len() {
            let rest = buf.get_mut(done..).unwrap_or_default();
            let n = file.seek_read(rest, offset + done as u64)?;
            if n == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            done += n;
        }
        Ok(())
    }
}

/// An install directory. Manifest paths (already validated by
/// `vgames_core::paths`) are joined component by component under a canonical
/// root; every directory on the way is created or checked without following
/// links, and existing targets must be regular files.
#[derive(Debug)]
pub struct SafeRoot {
    root: PathBuf,
    /// Directories already checked or created (relative, `/`-separated).
    known_dirs: Mutex<HashSet<String>>,
}

impl SafeRoot {
    /// Creates `root` (and missing parents) and canonicalizes it. On Windows the
    /// canonical form is an extended-length `\\?\` path.
    pub fn create(root: &Path) -> Result<Self, SafePathError> {
        fs::create_dir_all(root).map_err(|e| SafePathError::io(root, e))?;
        Self::open(root)
    }

    /// Opens an existing root.
    pub fn open(root: &Path) -> Result<Self, SafePathError> {
        let meta = fs::symlink_metadata(root).map_err(|e| SafePathError::io(root, e))?;
        if meta.file_type().is_symlink() {
            return Err(SafePathError::Link(root.to_owned()));
        }
        if !meta.is_dir() {
            return Err(SafePathError::NotADirectory(root.to_owned()));
        }
        let root = fs::canonicalize(root).map_err(|e| SafePathError::io(root, e))?;
        Ok(Self {
            root,
            known_dirs: Mutex::new(HashSet::new()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Absolute path of a manifest path (no filesystem access).
    pub fn path_of(&self, rel: &str) -> PathBuf {
        let mut path = self.root.clone();
        for part in rel.split('/') {
            path.push(part);
        }
        path
    }

    /// Creates the directory `rel` and its parents, refusing links and
    /// non-directories on the way. Returns its absolute path.
    pub fn ensure_dir(&self, rel: &str) -> Result<PathBuf, SafePathError> {
        let mut path = self.root.clone();
        let mut prefix = String::new();
        for part in rel.split('/') {
            path.push(part);
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if self.is_known(&prefix) {
                continue;
            }
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(SafePathError::Link(path));
                }
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => return Err(SafePathError::NotADirectory(path)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    match fs::create_dir(&path) {
                        Ok(()) => {}
                        // Created concurrently: re-check what it is.
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                            let meta = fs::symlink_metadata(&path)
                                .map_err(|e| SafePathError::io(&path, e))?;
                            if !meta.is_dir() || meta.file_type().is_symlink() {
                                return Err(SafePathError::NotADirectory(path));
                            }
                        }
                        Err(e) => return Err(SafePathError::io(&path, e)),
                    }
                }
                Err(e) => return Err(SafePathError::io(&path, e)),
            }
            self.remember(prefix.clone());
        }
        Ok(path)
    }

    /// Prepares the target of file `rel`: parent directories exist (checked as
    /// in [`Self::ensure_dir`]) and the file itself, if present, is a regular
    /// file. The canonical parent must still be inside the root.
    pub fn file_target(&self, rel: &str) -> Result<PathBuf, SafePathError> {
        let (parent, name) = match rel.rsplit_once('/') {
            Some((parent, name)) => (Some(parent), name),
            None => (None, rel),
        };
        let dir = match parent {
            Some(parent) => self.ensure_dir(parent)?,
            None => self.root.clone(),
        };
        let path = dir.join(name);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(SafePathError::Link(path)),
            Ok(meta) if !meta.is_file() => return Err(SafePathError::NotAFile(path)),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(SafePathError::io(&path, e)),
        }
        if parent.is_some() {
            let canonical = fs::canonicalize(&dir).map_err(|e| SafePathError::io(&dir, e))?;
            if !canonical.starts_with(&self.root) {
                return Err(SafePathError::Escapes(path));
            }
        }
        Ok(path)
    }

    fn is_known(&self, rel: &str) -> bool {
        self.known_dirs
            .lock()
            .map(|set| set.contains(rel))
            .unwrap_or(false)
    }

    fn remember(&self, rel: String) {
        if let Ok(mut set) = self.known_dirs.lock() {
            set.insert(rel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install.json");
        atomic_write(&path, b"one").unwrap();
        atomic_write(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert!(!dir.path().join("install.json.tmp").exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_does_not_follow_a_planted_temporary_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::write(outside.path(), b"untouched").unwrap();
        let path = dir.path().join("install.json");
        let planted = dir.path().join("install.json.tmp");
        std::os::unix::fs::symlink(outside.path(), &planted).unwrap();

        atomic_write(&path, b"record").unwrap();

        assert_eq!(fs::read(outside.path()).unwrap(), b"untouched");
        assert_eq!(fs::read(path).unwrap(), b"record");
        assert!(
            fs::symlink_metadata(planted)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn failed_atomic_write_removes_its_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install.json");
        fs::create_dir(&path).unwrap();

        assert!(atomic_write(&path, b"record").is_err());

        assert!(path.is_dir());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn creates_nested_targets() {
        let dir = tempfile::tempdir().unwrap();
        let root = SafeRoot::create(&dir.path().join("game")).unwrap();
        let target = root.file_target("Game/bin/game.exe").unwrap();
        assert!(target.parent().unwrap().is_dir());
        assert_eq!(target, root.path_of("Game/bin/game.exe"));
        // Idempotent.
        root.file_target("Game/bin/other.dll").unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_planted_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let root_path = dir.path().join("game");
        fs::create_dir(&root_path).unwrap();
        std::os::unix::fs::symlink(&outside, root_path.join("Game")).unwrap();
        let root = SafeRoot::open(&root_path).unwrap();
        assert!(matches!(
            root.file_target("Game/a.txt"),
            Err(SafePathError::Link(_))
        ));
        // A symlink in place of a file.
        fs::create_dir(root_path.join("Data")).unwrap();
        std::os::unix::fs::symlink(outside.join("x"), root_path.join("Data/x")).unwrap();
        assert!(matches!(
            root.file_target("Data/x"),
            Err(SafePathError::Link(_))
        ));
        // And the root itself.
        let link_root = dir.path().join("link-root");
        std::os::unix::fs::symlink(&outside, &link_root).unwrap();
        assert!(matches!(
            SafeRoot::open(&link_root),
            Err(SafePathError::Link(_))
        ));
        // Opening a symlinked file for writing fails too.
        fs::write(outside.join("x"), b"").unwrap();
        assert!(open_for_write(&root_path.join("Data/x")).is_err());
    }

    #[test]
    fn a_file_in_the_way_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = SafeRoot::create(dir.path()).unwrap();
        fs::write(dir.path().join("Game"), b"").unwrap();
        assert!(matches!(
            root.file_target("Game/a"),
            Err(SafePathError::NotADirectory(_))
        ));
        fs::create_dir(dir.path().join("dir")).unwrap();
        assert!(matches!(
            root.file_target("dir"),
            Err(SafePathError::NotAFile(_))
        ));
    }

    #[test]
    fn positional_io_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        fs::write(&path, vec![0u8; 16]).unwrap();
        let file = open_for_write(&path).unwrap();
        write_all_at(&file, b"abc", 5).unwrap();
        let file = File::open(&path).unwrap();
        let mut buf = [0u8; 4];
        read_exact_at(&file, &mut buf, 4).unwrap();
        assert_eq!(&buf, b"\0abc");
    }
}
