//! Filesystem helpers shared by the engines: atomic writes and an install
//! root that never follows symlinks or junctions (02-package-format §3: the
//! launcher re-checks every target path, defense in depth against links
//! planted in the install directory).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::Dir;

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

/// Opens an existing file for verification without following a final symlink.
pub fn open_for_read(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
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

/// An install directory, held open as a directory handle. Manifest paths (already validated by
/// `vgames_core::paths`) are resolved component by component *relative to that handle*, opening
/// each directory without following links (`openat` + `O_NOFOLLOW` on Unix, handle-relative
/// opens that refuse reparse points on Windows, through `cap-std`). A directory swapped for a
/// link or junction after a check therefore cannot redirect a write outside the install: the
/// next open through the handle refuses it (INS-07, Q12).
///
/// Use [`Self::open_file`] (or a [`Target`]) for every read and write of install content. The
/// path-returning methods ([`Self::file_target`], [`Self::existing_file`], [`Self::ensure_dir`])
/// check the same way but hand back a path, which the caller must not reopen for writing.
#[derive(Debug)]
pub struct SafeRoot {
    root: PathBuf,
    dir: Dir,
}

/// How [`SafeRoot::open_file`] opens a file. Never follows a final link either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// An existing file, read only.
    Read,
    /// An existing file, for positional writes.
    Write,
    /// Created (with missing parent folders) if absent, never truncated.
    CreateWrite,
}

fn classify(dir: &Dir, name: &str, path: &Path, error: io::Error) -> SafePathError {
    match dir.symlink_metadata(name) {
        Ok(meta) if meta.file_type().is_symlink() => SafePathError::Link(path.to_owned()),
        // A junction or other reparse point reads as neither a plain folder nor a file.
        Ok(meta) if !meta.is_dir() && !meta.is_file() => SafePathError::Link(path.to_owned()),
        _ => SafePathError::io(path, error),
    }
}

impl SafeRoot {
    /// Creates `root` (and missing parents) and opens it. On Windows the canonical form is an
    /// extended-length `\\?\` path.
    pub fn create(root: &Path) -> Result<Self, SafePathError> {
        fs::create_dir_all(root).map_err(|e| SafePathError::io(root, e))?;
        Self::open(root)
    }

    /// Opens an existing root (refusing a link or a non-folder).
    pub fn open(root: &Path) -> Result<Self, SafePathError> {
        let meta = fs::symlink_metadata(root).map_err(|e| SafePathError::io(root, e))?;
        if meta.file_type().is_symlink() {
            return Err(SafePathError::Link(root.to_owned()));
        }
        if !meta.is_dir() {
            return Err(SafePathError::NotADirectory(root.to_owned()));
        }
        let root = fs::canonicalize(root).map_err(|e| SafePathError::io(root, e))?;
        let dir = match (root.parent(), root.file_name()) {
            // Open the root itself through its parent without following a link, so the handle
            // is the folder that was checked.
            (Some(parent), Some(name)) => Dir::open_ambient_dir(parent, ambient_authority())
                .and_then(|parent| parent.open_dir_nofollow(name)),
            _ => Dir::open_ambient_dir(&root, ambient_authority()),
        }
        .map_err(|e| SafePathError::io(&root, e))?;
        Ok(Self { root, dir })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Absolute path of a manifest path (no filesystem access; for messages and logs).
    pub fn path_of(&self, rel: &str) -> PathBuf {
        let mut path = self.root.clone();
        for part in rel.split('/') {
            path.push(part);
        }
        path
    }

    /// The folder `rel` (`""` = the root) as a handle, each component opened without following
    /// links. Missing folders are created when `create`, else `None`.
    fn walk(&self, rel: &str, create: bool) -> Result<Option<Dir>, SafePathError> {
        let mut dir = self
            .dir
            .try_clone()
            .map_err(|e| SafePathError::io(&self.root, e))?;
        if rel.is_empty() {
            return Ok(Some(dir));
        }
        let mut path = self.root.clone();
        for part in rel.split('/') {
            path.push(part);
            let next = match dir.open_dir_nofollow(part) {
                Ok(next) => next,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    if !create {
                        return Ok(None);
                    }
                    match dir.create_dir(part) {
                        Ok(()) => {}
                        // Created concurrently: opening it below re-checks what it is.
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(e) => return Err(SafePathError::io(&path, e)),
                    }
                    dir.open_dir_nofollow(part)
                        .map_err(|e| classify(&dir, part, &path, e))?
                }
                Err(e) => {
                    return Err(match dir.symlink_metadata(part) {
                        Ok(meta) if meta.is_file() => SafePathError::NotADirectory(path),
                        _ => classify(&dir, part, &path, e),
                    });
                }
            };
            dir = next;
        }
        Ok(Some(dir))
    }

    /// Opens file `rel` relative to the root handle: every folder on the way is opened without
    /// following links, and so is the file itself, which must be a regular file. With
    /// [`Access::CreateWrite`] missing folders and the file are created; otherwise a missing
    /// component is `NotFound`.
    pub fn open_file(&self, rel: &str, access: Access) -> Result<File, SafePathError> {
        let (parent, name) = rel.rsplit_once('/').unwrap_or(("", rel));
        let path = self.path_of(rel);
        let create = access == Access::CreateWrite;
        let dir = self
            .walk(parent, create)?
            .ok_or_else(|| SafePathError::io(&path, io::Error::from(io::ErrorKind::NotFound)))?;
        // Not a FIFO or device: opening one could block or reach outside the file system.
        match dir.symlink_metadata(name) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(SafePathError::Link(path)),
            Ok(meta) if !meta.is_file() => return Err(SafePathError::NotAFile(path)),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound && create => {}
            Err(e) => return Err(SafePathError::io(&path, e)),
        }
        let mut options = cap_std::fs::OpenOptions::new();
        match access {
            Access::Read => options.read(true),
            Access::Write => options.write(true),
            Access::CreateWrite => options.write(true).create(true).truncate(false),
        };
        options.follow(FollowSymlinks::No);
        let file = dir
            .open_with(name, &options)
            .map_err(|e| classify(&dir, name, &path, e))?;
        let meta = file.metadata().map_err(|e| SafePathError::io(&path, e))?;
        if !meta.is_file() {
            return Err(SafePathError::NotAFile(path));
        }
        Ok(file.into_std())
    }

    /// Deletes file `rel` if present (a link there is removed itself, never followed). Folders
    /// on the way are opened as in [`Self::open_file`]. Returns whether something was deleted.
    pub fn remove_file(&self, rel: &str) -> Result<bool, SafePathError> {
        let (parent, name) = rel.rsplit_once('/').unwrap_or(("", rel));
        let path = self.path_of(rel);
        let Some(dir) = self.walk(parent, false)? else {
            return Ok(false);
        };
        match dir.remove_file_or_symlink(name) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(SafePathError::io(&path, e)),
        }
    }

    /// Creates the directory `rel` and its parents, refusing links and non-directories on the
    /// way. Returns its absolute path.
    pub fn ensure_dir(&self, rel: &str) -> Result<PathBuf, SafePathError> {
        self.walk(rel, true)?;
        Ok(self.path_of(rel))
    }

    /// Prepares the target of file `rel`: parent directories exist (created as in
    /// [`Self::ensure_dir`]) and the file itself, if present, is a regular file. Prefer
    /// [`Self::open_file`]: a path checked here can be swapped before it is reopened.
    pub fn file_target(&self, rel: &str) -> Result<PathBuf, SafePathError> {
        let (parent, name) = rel.rsplit_once('/').unwrap_or(("", rel));
        let path = self.path_of(rel);
        let dir = self
            .walk(parent, true)?
            .ok_or_else(|| SafePathError::io(&path, io::Error::from(io::ErrorKind::NotFound)))?;
        match dir.symlink_metadata(name) {
            Ok(meta) if meta.file_type().is_symlink() => Err(SafePathError::Link(path)),
            Ok(meta) if !meta.is_file() => Err(SafePathError::NotAFile(path)),
            Ok(_) => Ok(path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(path),
            Err(e) => Err(SafePathError::io(&path, e)),
        }
    }

    /// Finds an existing regular manifest file without creating missing directories. A missing
    /// component means the file needs repair. Prefer [`Self::open_file`] with [`Access::Read`].
    pub fn existing_file(&self, rel: &str) -> Result<Option<PathBuf>, SafePathError> {
        let (parent, name) = rel.rsplit_once('/').unwrap_or(("", rel));
        let path = self.path_of(rel);
        let Some(dir) = self.walk(parent, false)? else {
            return Ok(None);
        };
        match dir.symlink_metadata(name) {
            Ok(meta) if meta.file_type().is_symlink() => Err(SafePathError::Link(path)),
            Ok(meta) if !meta.is_file() => Err(SafePathError::NotAFile(path)),
            Ok(_) => Ok(Some(path)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(SafePathError::io(&path, e)),
        }
    }
}

/// Where one manifest file's bytes go: a file under a [`SafeRoot`], opened through the root's
/// handle every time (never by path).
#[derive(Debug, Clone)]
pub struct Target {
    root: Option<Arc<SafeRoot>>,
    rel: String,
}

impl Target {
    pub fn new(root: Arc<SafeRoot>, rel: impl Into<String>) -> Self {
        Self {
            root: Some(root),
            rel: rel.into(),
        }
    }

    /// A device such as `/dev/full`, opened by path: fault injection in tests only.
    #[cfg(feature = "testkit")]
    pub fn device_for_tests(path: impl Into<String>) -> Self {
        Self {
            root: None,
            rel: path.into(),
        }
    }

    /// The file, for positional writes.
    pub fn open_write(&self) -> io::Result<File> {
        match &self.root {
            Some(root) => root.open_file(&self.rel, Access::Write).map_err(into_io),
            None => open_for_write(Path::new(&self.rel)),
        }
    }

    /// For messages and logs only.
    pub fn display_path(&self) -> PathBuf {
        match &self.root {
            Some(root) => root.path_of(&self.rel),
            None => PathBuf::from(&self.rel),
        }
    }

    pub fn rel(&self) -> &str {
        &self.rel
    }
}

fn into_io(error: SafePathError) -> io::Error {
    match error {
        SafePathError::Io { source, .. } => source,
        other => io::Error::new(io::ErrorKind::PermissionDenied, other.to_string()),
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

    /// Points `link` at the folder `target`: a symbolic link on Unix, a junction on Windows (no
    /// privilege needed, and the reparse point an attacker would plant).
    fn link_dir(target: &Path, link: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd")
                .arg("/C")
                .arg("mklink")
                .arg("/J")
                .arg(link)
                .arg(target)
                .status()
                .unwrap();
            assert!(status.success(), "mklink /J failed");
        }
    }

    /// INS-07 (Q12): a folder checked (and even opened for a file) is swapped for a link to a
    /// folder outside the install; no later open, create, write or delete reaches outside.
    #[test]
    fn a_folder_swapped_for_a_link_after_the_check_never_lets_a_write_out() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir_all(outside.join("b")).unwrap();
        fs::write(outside.join("b/f.bin"), b"outside").unwrap();
        let root_path = dir.path().join("game");
        fs::create_dir(&root_path).unwrap();
        let root = Arc::new(SafeRoot::open(&root_path).unwrap());

        // Checked and prepared the normal way: folders created, the file allocated.
        root.open_file("a/b/f.bin", Access::CreateWrite).unwrap();
        assert!(root.file_target("a/b/g.bin").is_ok());
        let target = Target::new(Arc::clone(&root), "a/b/f.bin");

        // Between the checks and the writes: `a` becomes a link to `outside`.
        fs::rename(root_path.join("a"), root_path.join("a-moved")).unwrap();
        link_dir(&outside, &root_path.join("a"));

        assert!(target.open_write().is_err(), "a write through the link");
        for access in [Access::Read, Access::Write, Access::CreateWrite] {
            assert!(
                matches!(
                    root.open_file("a/b/f.bin", access),
                    Err(SafePathError::Link(_))
                ),
                "{access:?}"
            );
        }
        assert!(matches!(
            root.open_file("a/b/new.bin", Access::CreateWrite),
            Err(SafePathError::Link(_))
        ));
        assert!(matches!(
            root.ensure_dir("a/b/c"),
            Err(SafePathError::Link(_))
        ));
        assert!(matches!(
            root.file_target("a/b/g.bin"),
            Err(SafePathError::Link(_))
        ));
        assert!(matches!(
            root.remove_file("a/b/f.bin"),
            Err(SafePathError::Link(_))
        ));
        assert!(matches!(
            root.existing_file("a/b/f.bin"),
            Err(SafePathError::Link(_))
        ));

        // Nothing outside changed or appeared.
        assert_eq!(fs::read(outside.join("b/f.bin")).unwrap(), b"outside");
        let mut names: Vec<_> = fs::read_dir(outside.join("b"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["f.bin"]);
        assert!(!outside.join("b/c").exists());
    }

    /// The same swap one level down, after the parent folder was already walked: each open
    /// re-resolves from the root handle, so the deeper link is refused too.
    #[test]
    fn a_deeper_folder_swapped_for_a_link_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let root_path = dir.path().join("game");
        fs::create_dir(&root_path).unwrap();
        let root = SafeRoot::open(&root_path).unwrap();
        root.open_file("a/b/f.bin", Access::CreateWrite).unwrap();
        fs::rename(root_path.join("a/b"), root_path.join("a/b-moved")).unwrap();
        link_dir(&outside, &root_path.join("a/b"));
        assert!(matches!(
            root.open_file("a/b/f.bin", Access::CreateWrite),
            Err(SafePathError::Link(_))
        ));
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }

    /// A handle opened before the swap keeps writing to the file it opened (inside the install),
    /// never to whatever the path names now.
    #[test]
    fn an_open_handle_keeps_writing_inside_the_install() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir_all(outside.join("b")).unwrap();
        fs::write(outside.join("b/f.bin"), b"outside").unwrap();
        let root_path = dir.path().join("game");
        fs::create_dir(&root_path).unwrap();
        let root = SafeRoot::open(&root_path).unwrap();
        let handle = root.open_file("a/b/f.bin", Access::CreateWrite).unwrap();
        fs::rename(root_path.join("a"), root_path.join("a-moved")).unwrap();
        link_dir(&outside, &root_path.join("a"));
        write_all_at(&handle, b"inside", 0).unwrap();
        handle.sync_all().unwrap();
        assert_eq!(
            fs::read(root_path.join("a-moved/b/f.bin")).unwrap(),
            b"inside"
        );
        assert_eq!(fs::read(outside.join("b/f.bin")).unwrap(), b"outside");
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
