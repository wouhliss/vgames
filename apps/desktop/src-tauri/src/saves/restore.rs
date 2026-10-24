//! Backed-up, crash-safe restore (06 §2; 06-cloud-saves-notes "Ownership,
//! paths and durability"). Run on a blocking pool. The caller's sequence is
//! `Plan::new` → `back_up` → persist the returned `Backup` id as a recovery
//! record → `apply` → mark it complete; on any error, and on restart for
//! every incomplete record, `roll_back`. Each file is replaced through a
//! same-directory temporary file, fsync and rename, so a killed restore leaves
//! every file either whole-old or whole-new, and the backup restores the old.
use std::collections::BTreeSet;
use std::io::{self, Read, Write};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_core::paths;
use vgames_proto::saves::SaveFile;

use super::policy::{self, ProtectedInstalls};
use super::scan::{self, Error, Location};

/// Local backups kept per server, package and account (06 §3).
pub const KEEP_BACKUPS: usize = 5;
const MANIFEST: &str = "manifest.json";
const FILES: &str = "files";
const MAX_MANIFEST: u64 = 8 * 1024 * 1024;

/// A validated set of files to write (from a verified snapshot) and delete.
pub struct Plan {
    writes: Vec<SaveFile>,
    deletes: Vec<SaveFile>,
}
impl Plan {
    /// Runs the full `policy::preflight` over writes and deletes together.
    pub fn new(
        locations: &[Location],
        writes: Vec<SaveFile>,
        deletes: Vec<SaveFile>,
        protected: &ProtectedInstalls,
        cancel: &CancellationToken,
    ) -> Result<Self, Error> {
        let all: Vec<SaveFile> = writes.iter().chain(&deletes).cloned().collect();
        policy::preflight(locations, &all, protected, cancel)?;
        if all
            .iter()
            .any(|file| file.path.split('/').any(scan::is_temp_name))
        {
            return Err(Error::Invalid);
        }
        Ok(Self { writes, deletes })
    }
    fn targets(&self) -> impl Iterator<Item = &SaveFile> {
        self.writes.iter().chain(&self.deletes)
    }
}

/// What a backup holds: every target, with its contents before the restore.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backup {
    pub id: Uuid,
    pub entries: Vec<Entry>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub root: String,
    pub path: String,
    /// `None` when the file did not exist; roll back then removes it.
    pub original: Option<Original>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Original {
    pub size: u64,
    pub blake3: String,
}

/// Copies every existing target into `<backups>/<id>/files/<root>/<path>`
/// and writes the durable manifest last. `backups` is the scoped
/// `<app data>/save-backups/<server>/<package>/<account>` folder.
pub fn back_up(
    locations: &[Location],
    plan: &Plan,
    backups: &Dir,
    cancel: &CancellationToken,
) -> Result<Backup, Error> {
    let id = Uuid::now_v7();
    let folder = create_dirs(backups, &[&id.to_string(), FILES])?;
    let mut entries = Vec::new();
    for file in plan.targets() {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let location = location(locations, &file.root)?;
        let (dirs, name) = split(&file.path)?;
        let original = match target_parent(location, &dirs, false)? {
            None => None,
            Some(parent) => match open_read(&parent, name)? {
                None => None,
                Some(mut source) => {
                    let mut path = vec![file.root.as_str()];
                    path.extend(&dirs);
                    let into = create_dirs(&folder, &path)?;
                    Some(copy_new(&mut source, &into, name, cancel)?)
                }
            },
        };
        entries.push(Entry {
            root: file.root.clone(),
            path: file.path.clone(),
            original,
        });
    }
    let backup = Backup { id, entries };
    let json = serde_json::to_vec(&backup).map_err(|_| Error::Invalid)?;
    let own = backups.open_dir_nofollow(id.to_string())?;
    write_atomic(&own, MANIFEST, &mut json.as_slice(), None)?;
    sync_dir(backups)?;
    Ok(backup)
}

/// Writes every staged blob (named by its BLAKE3 in `staging`) over its
/// target and removes every deletion. Returns at the first error or
/// cancellation; the caller must then `roll_back`.
pub fn apply(
    locations: &[Location],
    plan: &Plan,
    staging: &Dir,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    for file in &plan.writes {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let location = location(locations, &file.root)?;
        let (dirs, name) = split(&file.path)?;
        let parent = target_parent(location, &dirs, true)?.ok_or(Error::Invalid)?;
        let mut source = open_read(staging, &file.blake3)?.ok_or(Error::Mismatch)?;
        let size = u64::try_from(file.size).map_err(|_| Error::Invalid)?;
        write_atomic(&parent, name, &mut source, Some((size, &file.blake3)))?;
    }
    for file in &plan.deletes {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let location = location(locations, &file.root)?;
        let (dirs, name) = split(&file.path)?;
        if let Some(parent) = target_parent(location, &dirs, false)? {
            remove(&parent, name)?;
        }
    }
    Ok(())
}

/// Puts every target back as the backup recorded it and removes leftover
/// temporary files. Idempotent: an interrupted roll back is simply rerun.
pub fn roll_back(locations: &[Location], backup: &Backup, backups: &Dir) -> Result<(), Error> {
    let files = backups.open_dir_nofollow(backup.id.to_string())?;
    let files = files.open_dir_nofollow(FILES)?;
    for entry in &backup.entries {
        let location = location(locations, &entry.root)?;
        let (dirs, name) = split(&entry.path)?;
        let parent = target_parent(location, &dirs, entry.original.is_some())?;
        if let Some(parent) = &parent {
            match &entry.original {
                Some(original) => {
                    let mut path = vec![entry.root.as_str()];
                    path.extend(&dirs);
                    let from = open_dirs(&files, &path)?;
                    let mut source = open_read(&from, name)?.ok_or(Error::Mismatch)?;
                    write_atomic(
                        parent,
                        name,
                        &mut source,
                        Some((original.size, &original.blake3)),
                    )?;
                }
                None => remove(parent, name)?,
            }
            remove_temps(parent)?;
        }
    }
    Ok(())
}

/// Reads and validates `<backups>/<id>/manifest.json`.
pub fn read_backup(backups: &Dir, id: Uuid) -> Result<Backup, Error> {
    let own = backups.open_dir_nofollow(id.to_string())?;
    let mut json = Vec::new();
    open_read(&own, MANIFEST)?
        .ok_or(Error::Invalid)?
        .take(MAX_MANIFEST + 1)
        .read_to_end(&mut json)?;
    if json.len() as u64 > MAX_MANIFEST {
        return Err(Error::Limit);
    }
    let backup: Backup = serde_json::from_slice(&json).map_err(|_| Error::Invalid)?;
    if backup.id != id || backup.entries.len() > 2 * scan::MAX_FILES {
        return Err(Error::Invalid);
    }
    for entry in &backup.entries {
        paths::validate_path(&entry.path).map_err(|_| Error::Invalid)?;
        if entry
            .original
            .as_ref()
            .is_some_and(|original| !scan::valid_hash(&original.blake3))
        {
            return Err(Error::Invalid);
        }
    }
    Ok(backup)
}

/// Removes all but the `keep` newest backups (UUIDv7 order) and any in
/// `protect`, such as one an unfinished restore still needs.
pub fn prune(backups: &Dir, keep: usize, protect: &[Uuid]) -> Result<(), Error> {
    let mut ids = Vec::new();
    for entry in backups.entries()? {
        let entry = entry?;
        let name = entry.file_name();
        if let Some(id) = name.to_str().and_then(|name| Uuid::parse_str(name).ok())
            && entry.file_type()?.is_dir()
        {
            ids.push(id);
        }
    }
    ids.sort_unstable_by(|a, b| b.cmp(a));
    let protect: BTreeSet<_> = protect.iter().collect();
    for id in ids.iter().skip(keep).filter(|id| !protect.contains(id)) {
        backups.remove_dir_all(id.to_string())?;
    }
    Ok(())
}

fn location<'a>(locations: &'a [Location], id: &str) -> Result<&'a Location, Error> {
    locations
        .iter()
        .find(|location| location.id == id)
        .ok_or(Error::Invalid)
}

fn split(path: &str) -> Result<(Vec<&str>, &str), Error> {
    let mut parts: Vec<&str> = path.split('/').collect();
    let name = parts.pop().ok_or(Error::Invalid)?;
    Ok((parts, name))
}

/// Opens (or with `create`, makes) the folder holding a target, without
/// following links. `Ok(None)` when it does not exist and `create` is off.
fn target_parent(location: &Location, dirs: &[&str], create: bool) -> Result<Option<Dir>, Error> {
    if create {
        std::fs::create_dir_all(&location.base)?;
    }
    let base = match Dir::open_ambient_dir(&location.base, cap_std::ambient_authority()) {
        Ok(dir) => dir,
        Err(error) if error.kind() == io::ErrorKind::NotFound && !create => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut path: Vec<&str> = location.folder.split('/').collect();
    path.extend(dirs);
    let mut dir = base;
    for component in path {
        dir = match dir.open_dir_nofollow(component) {
            Ok(child) => child,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if !create {
                    return Ok(None);
                }
                dir.create_dir(component)?;
                sync_dir(&dir)?;
                dir.open_dir_nofollow(component)?
            }
            Err(_) if is_link_or_file(&dir, component) => return Err(Error::Link),
            Err(error) => return Err(error.into()),
        };
    }
    Ok(Some(dir))
}

fn is_link_or_file(dir: &Dir, name: &str) -> bool {
    dir.symlink_metadata(name)
        .is_ok_and(|meta| meta.file_type().is_symlink() || !meta.is_dir())
}

fn create_dirs(root: &Dir, path: &[&str]) -> Result<Dir, Error> {
    let mut dir = root.try_clone()?;
    for component in path {
        match dir.create_dir(component) {
            Ok(()) => sync_dir(&dir)?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        dir = dir.open_dir_nofollow(component).map_err(|_| Error::Link)?;
    }
    Ok(dir)
}

fn open_dirs(root: &Dir, path: &[&str]) -> Result<Dir, Error> {
    let mut dir = root.try_clone()?;
    for component in path {
        dir = dir.open_dir_nofollow(component)?;
    }
    Ok(dir)
}

/// A regular file opened without following links; `None` when absent.
fn open_read(dir: &Dir, name: &str) -> Result<Option<std::fs::File>, Error> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    match dir.open_with(name, &options) {
        Ok(file) => {
            let file = file.into_std();
            if !file.metadata()?.is_file() {
                return Err(Error::Link);
            }
            Ok(Some(file))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) if is_link_or_file(dir, name) => Err(Error::Link),
        Err(error) => Err(error.into()),
    }
}

/// Copies into a new file and fsyncs it, returning what was copied.
fn copy_new(
    source: &mut impl Read,
    dir: &Dir,
    name: &str,
    cancel: &CancellationToken,
) -> Result<Original, Error> {
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    let mut out = dir.open_with(name, &options)?.into_std();
    let (size, blake3) = copy(source, &mut out, Some(cancel))?;
    out.sync_all()?;
    Ok(Original { size, blake3 })
}

fn copy(
    source: &mut impl Read,
    out: &mut std::fs::File,
    cancel: Option<&CancellationToken>,
) -> Result<(u64, String), Error> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 65_536];
    let mut size = 0u64;
    loop {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err(Error::Cancelled);
        }
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let chunk = buffer.get(..count).ok_or(Error::Invalid)?;
        size = size.checked_add(count as u64).ok_or(Error::Limit)?;
        if size > scan::MAX_BYTES {
            return Err(Error::Limit);
        }
        hasher.update(chunk);
        out.write_all(chunk)?;
    }
    Ok((size, hasher.finalize().to_hex().to_string()))
}

/// Same-directory temporary file → fsync → rename → fsync the directory.
/// With `expect`, a size or BLAKE3 mismatch leaves the target untouched.
/// Never cancelled midway: a started replacement always completes or fails.
fn write_atomic(
    dir: &Dir,
    name: &str,
    source: &mut impl Read,
    expect: Option<(u64, &str)>,
) -> Result<(), Error> {
    let temp = format!(".vgames-{}.tmp", Uuid::now_v7().simple());
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    let mut out = dir.open_with(&temp, &options)?.into_std();
    let result = copy(source, &mut out, None).and_then(|(size, hash)| {
        if expect.is_some_and(|(want_size, want_hash)| size != want_size || hash != want_hash) {
            return Err(Error::Mismatch);
        }
        out.sync_all()?;
        drop(out);
        if dir.symlink_metadata(name).is_ok_and(|meta| meta.is_dir()) {
            return Err(Error::Link);
        }
        dir.rename(&temp, dir, name)?;
        sync_dir(dir)
    });
    if result.is_err() {
        let _ = dir.remove_file(&temp);
    }
    result
}

fn remove(dir: &Dir, name: &str) -> Result<(), Error> {
    if dir.symlink_metadata(name).is_ok_and(|meta| meta.is_dir()) {
        return Err(Error::Link);
    }
    match dir.remove_file(name) {
        Ok(()) => sync_dir(dir),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn remove_temps(dir: &Dir) -> Result<(), Error> {
    for entry in dir.entries()? {
        let entry = entry?;
        if entry.file_name().to_str().is_some_and(scan::is_temp_name)
            && entry.file_type()?.is_file()
        {
            remove(dir, &entry.file_name().to_string_lossy())?;
        }
    }
    Ok(())
}

/// Makes a rename or removal durable. Windows has no directory fsync; NTFS
/// journals the metadata change itself.
fn sync_dir(dir: &Dir) -> Result<(), Error> {
    // cap-std's directory handles may be `O_PATH`, which cannot be fsynced.
    #[cfg(unix)]
    {
        let mut options = OpenOptions::new();
        options.read(true);
        dir.open_with(".", &options)?.into_std().sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(test)]
mod tests;
