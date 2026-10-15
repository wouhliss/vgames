//! Capability-relative scanning: symlinks/reparse points are never followed.
//! Run on a blocking pool. Metadata caches avoid hashing unchanged files.
use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, OpenOptions};
use globset::{Glob, GlobSet, GlobSetBuilder};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use vgames_core::{manifest::SaveLocation, paths};
use vgames_proto::saves::SaveFile;

pub const MAX_FILES: usize = 10_000;
pub const MAX_BYTES: u64 = 512 * 1024 * 1024;
const MAX_VISITED: usize = 100_000;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Cannot read save files.")]
    Io(#[from] io::Error),
    #[error("The save path or filter is invalid.")]
    Invalid,
    #[error("Save files contain a link or special file.")]
    Link,
    #[error("Save files exceed the allowed limit.")]
    Limit,
    #[error("A save file changed during scanning; retry is required.")]
    Changed,
    #[error("Save scanning cancelled.")]
    Cancelled,
}
pub struct Location {
    pub(super) id: String,
    pub(super) path_key: String,
    dir: Option<Dir>,
    pub(super) include: GlobSet,
    pub(super) exclude: GlobSet,
}
impl Location {
    /// `base` is supplied by the native known-folder/compatibility resolver,
    /// never by IPC. A missing base or declared folder is an empty location.
    pub fn open(config: &SaveLocation, base: &Path) -> Result<Self, Error> {
        if config.id.is_empty()
            || config.id.len() > 32
            || !config
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        {
            return Err(Error::Invalid);
        }
        paths::validate_path(&config.path).map_err(|_| Error::Invalid)?;
        let include = globs(&config.include)?;
        let exclude = globs(&config.exclude)?;
        let base = canonical_base(base)?;
        let path_key = paths::fold(
            &base
                .join(&config.path)
                .to_str()
                .ok_or(Error::Invalid)?
                .replace('\\', "/"),
        );
        let mut dir = match Dir::open_ambient_dir(base, cap_std::ambient_authority()) {
            Ok(dir) => Some(dir),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        for component in config.path.split('/') {
            let Some(parent) = dir else {
                break;
            };
            dir = match parent.open_dir_nofollow(component) {
                Ok(child) => Some(child),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(_)
                    if parent
                        .symlink_metadata(component)
                        .is_ok_and(|meta| meta.file_type().is_symlink() || !meta.is_dir()) =>
                {
                    return Err(Error::Link);
                }
                Err(error) => return Err(error.into()),
            };
        }
        Ok(Self {
            id: config.id.clone(),
            path_key,
            dir,
            include,
            exclude,
        })
    }
}
// Canonicalize the existing ancestor so absent known folders retain the same
// overlap identity as existing ones. Never create a folder during discovery.
pub(super) fn canonical_base(base: &Path) -> Result<PathBuf, Error> {
    if !base.is_absolute() {
        return Err(Error::Invalid);
    }
    let mut ancestor = base;
    let mut missing = Vec::new();
    loop {
        match std::fs::canonicalize(ancestor) {
            Ok(mut path) => {
                for component in missing.into_iter().rev() {
                    path.push(component);
                }
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // A dangling link is not an absent save directory.
                match std::fs::symlink_metadata(ancestor) {
                    Ok(_) => return Err(Error::Link),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                missing.push(ancestor.file_name().ok_or(Error::Invalid)?);
                ancestor = ancestor.parent().ok_or(Error::Invalid)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}
fn globs(patterns: &[String]) -> Result<GlobSet, Error> {
    let mut result = GlobSetBuilder::new();
    for pattern in patterns {
        result.add(Glob::new(pattern).map_err(|_| Error::Invalid)?);
    }
    result.build().map_err(|_| Error::Invalid)
}
#[derive(Debug, Default)]
pub struct Scan {
    pub files: BTreeMap<String, SaveFile>,
    pub hashed_files: usize,
    pub total_bytes: u64,
    visited: usize,
}
#[derive(Clone, Copy)]
struct Limits {
    files: usize,
    bytes: u64,
}
pub fn scan(
    locations: &[Location],
    base: &BTreeMap<String, SaveFile>,
    cancel: &CancellationToken,
) -> Result<Scan, Error> {
    scan_with_limits(
        locations,
        base,
        cancel,
        Limits {
            files: MAX_FILES,
            bytes: MAX_BYTES,
        },
    )
}
fn scan_with_limits(
    locations: &[Location],
    base: &BTreeMap<String, SaveFile>,
    cancel: &CancellationToken,
    limits: Limits,
) -> Result<Scan, Error> {
    let mut result = Scan::default();
    let mut ids = std::collections::BTreeSet::new();
    let mut roots: Vec<&str> = Vec::new();
    for location in locations {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if !ids.insert(&location.id) {
            return Err(Error::Invalid);
        }
        if roots.iter().any(|root| {
            *root == location.path_key
                || root.starts_with(&format!("{}/", location.path_key))
                || location.path_key.starts_with(&format!("{root}/"))
        }) {
            return Err(Error::Invalid);
        }
        roots.push(&location.path_key);
        if let Some(dir) = &location.dir {
            walk(location, dir, "", base, cancel, limits, &mut result)?;
        }
    }
    paths::validate_tree(result.files.keys().map(String::as_str), std::iter::empty())
        .map_err(|_| Error::Invalid)?;
    Ok(result)
}
fn walk(
    location: &Location,
    dir: &Dir,
    prefix: &str,
    base: &BTreeMap<String, SaveFile>,
    cancel: &CancellationToken,
    limits: Limits,
    result: &mut Scan,
) -> Result<(), Error> {
    for entry in dir.entries()? {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        result.visited += 1;
        if result.visited > MAX_VISITED {
            return Err(Error::Limit);
        }
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().ok_or(Error::Invalid)?;
        let relative = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        if location.exclude.is_match(&relative) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_symlink() || (!kind.is_dir() && !kind.is_file()) {
            return Err(Error::Link);
        }
        paths::validate_path(&relative).map_err(|_| Error::Invalid)?;
        if kind.is_dir() {
            let child = dir.open_dir_nofollow(name)?;
            walk(location, &child, &relative, base, cancel, limits, result)?;
        } else if location.include.is_empty() || location.include.is_match(&relative) {
            if result.files.len() >= limits.files {
                return Err(Error::Limit);
            }
            let meta = dir.symlink_metadata(name)?;
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Err(Error::Link);
            }
            let total = result
                .total_bytes
                .checked_add(meta.len())
                .ok_or(Error::Limit)?;
            if total > limits.bytes {
                return Err(Error::Limit);
            }
            let key = format!("{}/{relative}", location.id);
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No).nonblock(true);
            let mut file = dir.open_with(name, &options)?.into_std();
            let before = file.metadata()?;
            if !before.is_file() {
                return Err(Error::Link);
            }
            if before.len() != meta.len() {
                return Err(Error::Changed);
            }
            let mtime = timestamp(before.modified()?)?;
            let size = i64::try_from(before.len()).map_err(|_| Error::Limit)?;
            let cached = base.get(&key).filter(|old| {
                old.root == location.id
                    && old.path == relative
                    && old.size == size
                    && old.mtime == mtime
                    && valid_hash(&old.blake3)
            });
            let hash = if let Some(cached) = cached {
                cached.blake3.clone()
            } else {
                let mut hasher = blake3::Hasher::new();
                let mut buffer = [0u8; 65_536];
                let mut read = 0u64;
                loop {
                    if cancel.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    let count = file.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    read = read.checked_add(count as u64).ok_or(Error::Limit)?;
                    if read > before.len() {
                        return Err(Error::Changed);
                    }
                    hasher.update(buffer.get(..count).ok_or(Error::Invalid)?);
                }
                if read != before.len() {
                    return Err(Error::Changed);
                }
                result.hashed_files += 1;
                hasher.finalize().to_hex().to_string()
            };
            let after = file.metadata()?;
            if after.len() != before.len() || after.modified()? != before.modified()? {
                return Err(Error::Changed);
            }
            result.total_bytes = total;
            if result
                .files
                .insert(
                    key,
                    SaveFile {
                        root: location.id.clone(),
                        path: relative,
                        size,
                        blake3: hash,
                        mtime,
                    },
                )
                .is_some()
            {
                return Err(Error::Invalid);
            }
        }
    }
    Ok(())
}
pub(super) fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn timestamp(value: SystemTime) -> Result<OffsetDateTime, Error> {
    let nanos = match value.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(duration) => i128::try_from(duration.as_nanos()).map_err(|_| Error::Invalid)?,
        Err(error) => -i128::try_from(error.duration().as_nanos()).map_err(|_| Error::Invalid)?,
    };
    OffsetDateTime::from_unix_timestamp_nanos(nanos).map_err(|_| Error::Invalid)
}
#[cfg(test)]
mod tests;
