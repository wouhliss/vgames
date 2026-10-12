//! Path-safe extraction of runtime archives (09-compatibility §5, GAME-03).
//!
//! Runs on a blocking thread, into an empty directory the caller renames into place.
//! Every write goes through a handle on that directory (`cap_std::fs::Dir`), so a
//! path component swapped for a link meanwhile cannot redirect a write, and modes
//! are set on the open file handle. The whole archive is refused when any entry:
//!
//! - has an absolute path, a drive or root prefix, or a `..` component;
//! - is a device node, a FIFO or anything other than a file, a directory, or a link;
//! - is a symbolic link whose target is absolute, or which, resolved through every
//!   other link of the archive (forward references included), leaves the target or
//!   loops (more than [`MAX_LINK_HOPS`] links);
//! - is a hard link to anything but a regular file already extracted;
//! - would be written through a link extracted earlier, or replaces an earlier entry;
//! - pushes the archive past [`MAX_ENTRIES`] entries or [`MAX_EXTRACTED_BYTES`].
//!
//! Permissions keep only the owner/group/other read, write and execute bits (no
//! setuid, setgid or sticky bit). Links are created only on Unix (runtimes are for
//! Linux and macOS); elsewhere a link refuses the archive.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use cap_std::fs::Dir;
use vgames_core::runtimes::ArchiveFormat;

/// Entries per archive (GE-Proton has about 15,000).
pub const MAX_ENTRIES: usize = 200_000;
/// Bytes written per archive (a decompression bomb stops here).
pub const MAX_EXTRACTED_BYTES: u64 = 32 * 1024 * 1024 * 1024;
/// Links followed while resolving one link (the kernel's own limit is 40).
pub const MAX_LINK_HOPS: usize = 40;

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("archive entry {path:?} is refused: {reason}")]
    Unsafe { path: String, reason: &'static str },
    #[error("the archive has more than {MAX_ENTRIES} entries")]
    TooManyEntries,
    #[error("the archive expands beyond {MAX_EXTRACTED_BYTES} bytes")]
    TooLarge,
    #[error("the archive is damaged: {0}")]
    Corrupt(String),
    #[error("cannot write the runtime: {0}")]
    Io(#[from] io::Error),
}

fn unsafe_entry(path: &Path, reason: &'static str) -> ExtractError {
    ExtractError::Unsafe {
        path: path.to_string_lossy().into_owned(),
        reason,
    }
}

/// Extracts `archive` (in `format`) into `dest`, which must exist and be empty.
/// On an error the caller removes `dest`.
pub fn extract(archive: &Path, format: ArchiveFormat, dest: &Path) -> Result<(), ExtractError> {
    let file = File::open(archive)?;
    let mut sink = Sink::open(dest)?;
    match format {
        ArchiveFormat::TarGz => extract_tar(flate2::read::GzDecoder::new(file), &mut sink)?,
        ArchiveFormat::TarXz => extract_tar(liblzma::read::XzDecoder::new(file), &mut sink)?,
        ArchiveFormat::TarZst => extract_tar(
            zstd::stream::read::Decoder::new(file)
                .map_err(|e| ExtractError::Corrupt(e.to_string()))?,
            &mut sink,
        )?,
        ArchiveFormat::Zip => extract_zip(file, &mut sink)?,
    }
    sink.check_links()
}

/// A relative path made only of normal components; anything else refuses the archive.
fn relative(path: &Path) -> Result<PathBuf, ExtractError> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir => return Err(unsafe_entry(path, "contains `..`")),
            Component::RootDir | Component::Prefix(_) => {
                return Err(unsafe_entry(path, "is an absolute path"));
            }
        }
    }
    Ok(out)
}

/// Resolves `link` (relative to the target root) through `links` the way the
/// kernel would: `Err` when the resolution leaves the root, meets an absolute
/// target, or follows more than [`MAX_LINK_HOPS`] links.
fn resolve_link(links: &HashMap<PathBuf, PathBuf>, link: &Path) -> Result<(), &'static str> {
    let mut current: Vec<OsString> = link
        .parent()
        .map(|p| p.iter().map(ToOwned::to_owned).collect())
        .unwrap_or_default();
    let mut pending: VecDeque<Component<'_>> = VecDeque::new();
    let start = links.get(link).ok_or("is not a link")?;
    pending.extend(start.components());
    let mut hops = 1;
    while let Some(component) = pending.pop_front() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if current.pop().is_none() {
                    return Err("links outside the runtime");
                }
            }
            Component::RootDir | Component::Prefix(_) => return Err("links to an absolute path"),
            Component::Normal(part) => {
                current.push(part.to_owned());
                let here: PathBuf = current.iter().collect();
                if let Some(target) = links.get(&here) {
                    hops += 1;
                    if hops > MAX_LINK_HOPS {
                        return Err("is part of a link loop");
                    }
                    current.pop();
                    for c in target.components().rev() {
                        pending.push_front(c);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Where each entry lands, and what was created so far.
struct Sink {
    root: Dir,
    entries: usize,
    written: u64,
    /// Extracted links (relative path → target): nothing is written below them,
    /// and each is resolved through the others once the archive is complete.
    links: HashMap<PathBuf, PathBuf>,
    /// Relative paths of extracted regular files (hard-link targets).
    files: HashSet<PathBuf>,
}

impl Sink {
    fn open(dest: &Path) -> Result<Self, ExtractError> {
        Ok(Self {
            root: Dir::open_ambient_dir(dest, cap_std::ambient_authority())?,
            entries: 0,
            written: 0,
            links: HashMap::new(),
            files: HashSet::new(),
        })
    }

    /// Counts the entry and returns its relative path, checked against earlier links.
    fn admit(&mut self, raw: &Path) -> Result<Option<PathBuf>, ExtractError> {
        self.entries += 1;
        if self.entries > MAX_ENTRIES {
            return Err(ExtractError::TooManyEntries);
        }
        let rel = relative(raw)?;
        if rel.as_os_str().is_empty() {
            return Ok(None); // "./"
        }
        if rel.ancestors().skip(1).any(|a| self.links.contains_key(a)) {
            return Err(unsafe_entry(raw, "is below a link"));
        }
        if self.links.contains_key(&rel) || self.files.contains(&rel) {
            return Err(unsafe_entry(raw, "replaces an earlier entry"));
        }
        Ok(Some(rel))
    }

    fn parent_of(&self, rel: &Path) -> io::Result<()> {
        match rel.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => self.root.create_dir_all(parent),
            _ => Ok(()),
        }
    }

    fn dir(&mut self, rel: &Path) -> Result<(), ExtractError> {
        self.root.create_dir_all(rel)?;
        Ok(())
    }

    fn file(
        &mut self,
        rel: &Path,
        mode: Option<u32>,
        data: &mut dyn Read,
    ) -> Result<(), ExtractError> {
        self.parent_of(rel)?;
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut out = self.root.open_with(rel, &options)?.into_std();
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = data
                .read(&mut buf)
                .map_err(|e| ExtractError::Corrupt(e.to_string()))?;
            if n == 0 {
                break;
            }
            self.written = self.written.saturating_add(n as u64);
            if self.written > MAX_EXTRACTED_BYTES {
                return Err(ExtractError::TooLarge);
            }
            out.write_all(buf.get(..n).unwrap_or_default())?;
        }
        out.flush()?;
        set_mode(&out, mode)?;
        self.files.insert(rel.to_owned());
        Ok(())
    }

    fn symlink(&mut self, raw: &Path, rel: &Path, target: &Path) -> Result<(), ExtractError> {
        if target.as_os_str().is_empty() {
            return Err(unsafe_entry(raw, "is a link without a target"));
        }
        if target.has_root()
            || target
                .components()
                .any(|c| matches!(c, Component::Prefix(_)))
        {
            return Err(unsafe_entry(raw, "links to an absolute path"));
        }
        self.parent_of(rel)?;
        self.make_symlink(raw, target, rel)?;
        self.links.insert(rel.to_owned(), target.to_owned());
        Ok(())
    }

    #[cfg(unix)]
    fn make_symlink(&self, _raw: &Path, target: &Path, rel: &Path) -> Result<(), ExtractError> {
        // The target is checked in `check_links`, through every link of the archive.
        self.root.symlink_contents(target, rel)?;
        Ok(())
    }

    #[cfg(not(unix))]
    fn make_symlink(&self, raw: &Path, _target: &Path, _rel: &Path) -> Result<(), ExtractError> {
        Err(unsafe_entry(
            raw,
            "is a link (runtimes are for Linux and macOS)",
        ))
    }

    fn hardlink(&mut self, raw: &Path, rel: &Path, target: &Path) -> Result<(), ExtractError> {
        let source = relative(target)?;
        if !self.files.contains(&source) {
            return Err(unsafe_entry(
                raw,
                "hard-links to something that is not an extracted file",
            ));
        }
        self.parent_of(rel)?;
        self.root.hard_link(&source, &self.root, rel)?;
        self.files.insert(rel.to_owned());
        Ok(())
    }

    /// Resolves every link through the complete set (forward references included).
    fn check_links(&self) -> Result<(), ExtractError> {
        let mut links: Vec<&PathBuf> = self.links.keys().collect();
        links.sort();
        for link in links {
            resolve_link(&self.links, link).map_err(|reason| unsafe_entry(link, reason))?;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn set_mode(file: &File, mode: Option<u32>) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = mode.unwrap_or(0o644) & 0o777;
    file.set_permissions(std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_file: &File, _mode: Option<u32>) -> io::Result<()> {
    Ok(())
}

fn extract_tar<R: Read>(reader: R, sink: &mut Sink) -> Result<(), ExtractError> {
    let mut archive = tar::Archive::new(reader);
    let entries = archive
        .entries()
        .map_err(|e| ExtractError::Corrupt(e.to_string()))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| ExtractError::Corrupt(e.to_string()))?;
        let raw = entry
            .path()
            .map_err(|e| ExtractError::Corrupt(e.to_string()))?
            .into_owned();
        let kind = entry.header().entry_type();
        // Metadata entries (pax, GNU long names) are consumed by the reader.
        if kind.is_pax_global_extensions() || kind.is_pax_local_extensions() {
            continue;
        }
        let Some(rel) = sink.admit(&raw)? else {
            continue;
        };
        if kind.is_dir() {
            sink.dir(&rel)?;
        } else if kind.is_file() || kind == tar::EntryType::Continuous {
            let mode = entry.header().mode().ok();
            sink.file(&rel, mode, &mut entry)?;
        } else if kind.is_symlink() {
            let target = entry
                .link_name()
                .map_err(|e| ExtractError::Corrupt(e.to_string()))?
                .ok_or_else(|| unsafe_entry(&raw, "is a link without a target"))?
                .into_owned();
            sink.symlink(&raw, &rel, &target)?;
        } else if kind.is_hard_link() {
            let target = entry
                .link_name()
                .map_err(|e| ExtractError::Corrupt(e.to_string()))?
                .ok_or_else(|| unsafe_entry(&raw, "is a link without a target"))?
                .into_owned();
            sink.hardlink(&raw, &rel, &target)?;
        } else {
            return Err(unsafe_entry(&raw, "is a device, FIFO or special file"));
        }
    }
    Ok(())
}

/// Unix file-type bits in a zip entry's external attributes.
const S_IFMT: u32 = 0o170_000;
const S_IFREG: u32 = 0o100_000;
const S_IFDIR: u32 = 0o040_000;
const S_IFLNK: u32 = 0o120_000;

fn extract_zip(file: File, sink: &mut Sink) -> Result<(), ExtractError> {
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| ExtractError::Corrupt(e.to_string()))?;
    if archive.len() > MAX_ENTRIES {
        return Err(ExtractError::TooManyEntries);
    }
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|e| ExtractError::Corrupt(e.to_string()))?;
        // The raw name, not `enclosed_name`: an unsafe name refuses the archive
        // instead of being skipped.
        let raw = PathBuf::from(entry.name().replace('\\', "/"));
        let Some(rel) = sink.admit(&raw)? else {
            continue;
        };
        let mode = entry.unix_mode();
        let kind = mode.map(|m| m & S_IFMT);
        if entry.is_dir() || kind == Some(S_IFDIR) {
            sink.dir(&rel)?;
        } else if kind == Some(S_IFLNK) {
            let mut target = String::new();
            entry
                .by_ref()
                .take(4096)
                .read_to_string(&mut target)
                .map_err(|e| ExtractError::Corrupt(e.to_string()))?;
            sink.symlink(&raw, &rel, Path::new(&target))?;
        } else if kind.is_none() || kind == Some(S_IFREG) || kind == Some(0) {
            sink.file(&rel, mode, &mut entry)?;
        } else {
            return Err(unsafe_entry(&raw, "is a device, FIFO or special file"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
