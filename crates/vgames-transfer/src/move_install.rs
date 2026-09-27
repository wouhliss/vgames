//! Move an installed package: rename on one filesystem, or copy every file,
//! verify the copy, and delete the source only after destination commit.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use vgames_core::manifest::Manifest;

use crate::fsutil::{self, SafeRoot};
use crate::install::{self, InstallError, InstallState};
use crate::update::commit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveKind {
    Renamed,
    Copied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveReport {
    pub kind: MoveKind,
    pub copied_bytes: u64,
    pub files_verified: u64,
}

/// `destination` must be a new child of a configured library root. The caller
/// must have verified `manifest` and stopped any running game first. This is
/// blocking filesystem work; call it from a blocking pool in async contexts.
pub fn move_install(
    source: &Path,
    destination: &Path,
    manifest: &Manifest,
) -> Result<MoveReport, InstallError> {
    move_inner(source, destination, manifest, false)
}

fn move_inner(
    source: &Path,
    destination: &Path,
    manifest: &Manifest,
    force_copy: bool,
) -> Result<MoveReport, InstallError> {
    let safe = SafeRoot::open(source)?;
    if commit::is_pending(safe.root())? {
        return Err(InstallError::Conflict(
            "finish the interrupted update before moving".into(),
        ));
    }
    let record = install::read_record(safe.root())?
        .ok_or_else(|| InstallError::Conflict("missing install record".into()))?;
    if record.state != InstallState::Installed
        || record.server_id != manifest.server_id
        || record.package_id != manifest.package_id
        || record.version_id != manifest.version_id
    {
        return Err(InstallError::Conflict(
            "installed release does not match the move".into(),
        ));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| InstallError::Conflict("destination has no library folder".into()))?;
    let name = destination
        .file_name()
        .ok_or_else(|| InstallError::Conflict("destination has no folder name".into()))?;
    if !matches!(
        Path::new(name).components().next(),
        Some(Component::Normal(_))
    ) || parent.join(name) != destination
    {
        return Err(InstallError::Conflict(
            "invalid destination folder name".into(),
        ));
    }
    let library = SafeRoot::open(parent)?;
    if library.root().starts_with(safe.root()) || library.root() == safe.root() {
        return Err(InstallError::Conflict(
            "destination is inside the install".into(),
        ));
    }
    let destination = library.root().join(name);
    if fs::symlink_metadata(&destination).is_ok() {
        return Err(InstallError::Conflict("destination already exists".into()));
    }
    let entries = scan_tree(safe.root())?;
    if !force_copy {
        match fs::rename(safe.root(), &destination) {
            Ok(()) => {
                fsutil::sync_dir(library.root())
                    .map_err(|error| InstallError::io("flush", library.root(), error))?;
                if let Some(parent) = safe.root().parent() {
                    fsutil::sync_dir(parent)
                        .map_err(|error| InstallError::io("flush", parent, error))?;
                }
                return Ok(MoveReport {
                    kind: MoveKind::Renamed,
                    copied_bytes: 0,
                    files_verified: 0,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {}
            Err(error) => return Err(InstallError::io("move", &destination, error)),
        }
    }
    let mut expected = HashMap::new();
    for file in &manifest.files {
        expected.insert(
            safe.path_of(&file.path),
            (file.size, *file.blake3.as_bytes()),
        );
    }
    let (staging, mut guard) = temporary_destination(&destination)?;
    let mut report = MoveReport {
        kind: MoveKind::Copied,
        copied_bytes: 0,
        files_verified: 0,
    };
    for entry in entries {
        let relative = entry
            .strip_prefix(safe.root())
            .map_err(|_| InstallError::Conflict("source path escaped the install".into()))?;
        let target = staging.join(relative);
        let metadata = fs::symlink_metadata(&entry)
            .map_err(|error| InstallError::io("inspect", &entry, error))?;
        if metadata.file_type().is_symlink() {
            return Err(InstallError::Conflict(format!(
                "{} changed to a link during move",
                entry.display()
            )));
        }
        if metadata.is_dir() {
            fs::create_dir(&target).map_err(|error| InstallError::io("create", &target, error))?;
        } else if metadata.is_file() {
            let copied = copy_and_verify(&entry, &target, expected.remove(&entry))?;
            report.copied_bytes = report
                .copied_bytes
                .checked_add(copied)
                .ok_or_else(|| InstallError::Internal("move size overflow".into()))?;
            report.files_verified += 1;
        } else {
            return Err(InstallError::Conflict(format!(
                "{} is not a regular file",
                entry.display()
            )));
        }
    }
    if !expected.is_empty() {
        return Err(InstallError::Conflict(
            "a signed file is missing from the source install".into(),
        ));
    }
    sync_tree_dirs(&staging)?;
    fs::rename(&staging, &destination)
        .map_err(|error| InstallError::io("move", &destination, error))?;
    guard.0 = None;
    fsutil::sync_dir(library.root())
        .map_err(|error| InstallError::io("flush", library.root(), error))?;
    install::remove_tree_no_follow(safe.root())?;
    if let Some(parent) = safe.root().parent() {
        fsutil::sync_dir(parent).map_err(|error| InstallError::io("flush", parent, error))?;
    }
    Ok(report)
}

/// Directory-first traversal, without following or accepting links/special files.
fn scan_tree(root: &Path) -> Result<Vec<PathBuf>, InstallError> {
    let mut entries = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(dir) = pending.pop() {
        let mut children = fs::read_dir(&dir)
            .map_err(|error| InstallError::io("read", &dir, error))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()
            .map_err(|error| InstallError::io("read", &dir, error))?;
        children.sort();
        for child in children {
            let meta = fs::symlink_metadata(&child)
                .map_err(|error| InstallError::io("inspect", &child, error))?;
            if meta.file_type().is_symlink() || (!meta.is_dir() && !meta.is_file()) {
                return Err(InstallError::Conflict(format!(
                    "{} is a link or special file",
                    child.display()
                )));
            }
            if meta.is_dir() {
                pending.push(child.clone());
            }
            entries.push(child);
        }
    }
    entries.sort_by_key(|path| path.components().count());
    Ok(entries)
}

struct PendingDir(Option<PathBuf>);

impl Drop for PendingDir {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = install::remove_tree_no_follow(path);
        }
    }
}

fn temporary_destination(destination: &Path) -> Result<(PathBuf, PendingDir), InstallError> {
    loop {
        let mut name = destination.file_name().unwrap_or_default().to_owned();
        name.push(format!(".vgames-move-{}", uuid::Uuid::now_v7()));
        let path = destination.with_file_name(name);
        match fs::create_dir(&path) {
            Ok(()) => return Ok((path.clone(), PendingDir(Some(path)))),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(InstallError::io("create", &path, error)),
        }
    }
}

fn copy_and_verify(
    source: &Path,
    target: &Path,
    expected: Option<(u64, [u8; 32])>,
) -> Result<u64, InstallError> {
    let mut reader =
        fsutil::open_for_read(source).map_err(|error| InstallError::io("open", source, error))?;
    let mut writer = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(|error| InstallError::io("create", target, error))?;
    let mut buffer = [0u8; 256 * 1024];
    let mut source_hash = blake3::Hasher::new();
    let mut size = 0u64;
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| InstallError::io("read", source, error))?;
        if count == 0 {
            break;
        }
        let part = buffer.get(..count).unwrap_or_default();
        writer
            .write_all(part)
            .map_err(|error| InstallError::io("write", target, error))?;
        source_hash.update(part);
        size += count as u64;
    }
    if let Some((wanted_size, wanted_hash)) = expected
        && (size != wanted_size || source_hash.finalize().as_bytes() != &wanted_hash)
    {
        return Err(InstallError::Conflict(format!(
            "{} no longer matches the signed release",
            source.display()
        )));
    }
    writer
        .set_permissions(
            reader
                .metadata()
                .map_err(|error| InstallError::io("inspect", source, error))?
                .permissions(),
        )
        .map_err(|error| InstallError::io("set permissions of", target, error))?;
    writer
        .sync_all()
        .map_err(|error| InstallError::io("flush", target, error))?;
    let destination_hash = hash_file(target)?;
    if destination_hash != *source_hash.finalize().as_bytes()
        || hash_file(source)? != destination_hash
    {
        return Err(InstallError::Conflict(format!(
            "{} changed during move",
            target.display()
        )));
    }
    Ok(size)
}

fn hash_file(path: &Path) -> Result<[u8; 32], InstallError> {
    let mut file =
        fsutil::open_for_read(path).map_err(|error| InstallError::io("open", path, error))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 256 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| InstallError::io("read", path, error))?;
        if count == 0 {
            break;
        }
        hasher.update(buffer.get(..count).unwrap_or_default());
    }
    Ok(*hasher.finalize().as_bytes())
}

fn sync_tree_dirs(root: &Path) -> Result<(), InstallError> {
    let mut dirs = scan_tree(root)?
        .into_iter()
        .filter(|path| fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()))
        .collect::<Vec<_>>();
    dirs.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    dirs.push(root.to_owned());
    for dir in dirs {
        fsutil::sync_dir(&dir).map_err(|error| InstallError::io("flush", &dir, error))?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::testkit::package::{FileSpec, TestPackage, write_tree};
    use vgames_pack::Compression;

    fn setup() -> (tempfile::TempDir, tempfile::TempDir, TestPackage) {
        let source_library = tempfile::tempdir().unwrap();
        let dest_library = tempfile::tempdir().unwrap();
        let file = FileSpec::random("bin/game", 64, 8);
        let package = TestPackage::build(std::slice::from_ref(&file), &[], Compression::None);
        let root = source_library.path().join("game");
        write_tree(&root, &[file], &[]);
        fs::create_dir(root.join(".vgames")).unwrap();
        fs::write(root.join(".vgames/manifest.json"), &package.manifest).unwrap();
        fs::write(
            root.join(".vgames/manifest.sig"),
            package.envelope.to_bytes(),
        )
        .unwrap();
        install::write_record(
            &root,
            &install::InstallRecord::for_manifest(
                &package.release().verified,
                InstallState::Installed,
            ),
        )
        .unwrap();
        (source_library, dest_library, package)
    }

    #[test]
    fn same_filesystem_renames_the_entire_install() {
        let (source_library, dest_library, package) = setup();
        let source = source_library.path().join("game");
        fs::write(source.join("player.ini"), b"local settings").unwrap();
        let dest = dest_library.path().join("game");
        let report = move_install(&source, &dest, package.release().manifest()).unwrap();
        assert_eq!(report.kind, MoveKind::Renamed);
        assert!(!source.exists());
        assert_eq!(
            fs::read(dest.join("player.ini")).unwrap(),
            b"local settings"
        );
    }

    #[test]
    fn copy_mode_verifies_every_file_before_deleting_source() {
        let (source_library, dest_library, package) = setup();
        let source = source_library.path().join("game");
        fs::write(source.join("player.ini"), b"local settings").unwrap();
        let dest = dest_library.path().join("game");
        let report = move_inner(&source, &dest, package.release().manifest(), true).unwrap();
        assert_eq!(report.kind, MoveKind::Copied);
        assert!(report.files_verified >= 4); // game, settings, and launcher metadata
        assert!(!source.exists());
        assert_eq!(
            fs::read(dest.join("player.ini")).unwrap(),
            b"local settings"
        );
        assert_eq!(
            fs::read(dest.join("bin/game")).unwrap(),
            package.files[0].bytes()
        );
    }

    #[test]
    fn damaged_signed_file_keeps_source_and_does_not_publish_a_copy() {
        let (source_library, dest_library, package) = setup();
        let source = source_library.path().join("game");
        fs::write(source.join("bin/game"), b"wrong bytes").unwrap();
        let dest = dest_library.path().join("game");
        assert!(move_inner(&source, &dest, package.release().manifest(), true).is_err());
        assert!(source.exists());
        assert!(!dest.exists());
    }

    #[test]
    fn missing_signed_file_keeps_source_and_does_not_publish_a_copy() {
        let (source_library, dest_library, package) = setup();
        let source = source_library.path().join("game");
        fs::remove_file(source.join("bin/game")).unwrap();
        let dest = dest_library.path().join("game");
        assert!(move_inner(&source, &dest, package.release().manifest(), true).is_err());
        assert!(source.exists());
        assert!(!dest.exists());
    }

    #[cfg(unix)]
    #[test]
    fn planted_link_is_refused_before_rename() {
        use std::os::unix::fs::symlink;
        let (source_library, dest_library, package) = setup();
        let source = source_library.path().join("game");
        symlink("/tmp", source.join("link")).unwrap();
        let dest = dest_library.path().join("game");
        assert!(move_install(&source, &dest, package.release().manifest()).is_err());
        assert!(source.exists());
        assert!(!dest.exists());
    }
}
