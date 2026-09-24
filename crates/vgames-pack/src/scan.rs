//! Native folder scan and file reader (not available in WASM builds).

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::plan::SourceFile;
use crate::source::{SourceError, SourceReader};

/// Result of scanning a folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    pub files: Vec<SourceFile>,
    /// Empty folders (recreated on install).
    pub directories: Vec<String>,
    /// Entries that cannot be packed, with the reason (shown in the plan preview).
    pub rejected: Vec<Rejected>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub path: String,
    pub reason: RejectReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    Symlink,
    SpecialFile,
    NotUtf8,
}

fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    match meta.modified() {
        Ok(t) => match t.duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
            Err(e) => -i64::try_from(e.duration().as_millis()).unwrap_or(i64::MAX),
        },
        Err(_) => 0,
    }
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    false
}

fn relative(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    Some(parts?.join("/"))
}

/// Walks `root` without following symlinks. Paths are not validated here;
/// [`crate::plan::Plan::new`] does that.
pub fn scan(root: &Path) -> io::Result<Scan> {
    let mut out = Scan::default();
    for entry in walkdir::WalkDir::new(root).follow_links(false).min_depth(1) {
        let entry = entry.map_err(io::Error::other)?;
        let lossy = || {
            entry
                .path()
                .strip_prefix(root)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .replace('\\', "/")
        };
        let Some(path) = relative(root, entry.path()) else {
            out.rejected.push(Rejected {
                path: lossy(),
                reason: RejectReason::NotUtf8,
            });
            continue;
        };
        let file_type = entry.file_type();
        if file_type.is_symlink() {
            out.rejected.push(Rejected {
                path,
                reason: RejectReason::Symlink,
            });
        } else if file_type.is_dir() {
            if std::fs::read_dir(entry.path())?.next().is_none() {
                out.directories.push(path);
            }
        } else if file_type.is_file() {
            let meta = entry.metadata().map_err(io::Error::other)?;
            out.files.push(SourceFile {
                path,
                size: meta.len(),
                mtime_ms: mtime_ms(&meta),
                executable: is_executable(&meta),
            });
        } else {
            out.rejected.push(Rejected {
                path,
                reason: RejectReason::SpecialFile,
            });
        }
    }
    Ok(out)
}

/// Reads planned files under a root, refusing any that changed since the scan.
#[derive(Debug, Clone)]
pub struct FsReader {
    root: PathBuf,
}

impl FsReader {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn check(file: &SourceFile, meta: &std::fs::Metadata) -> io::Result<()> {
        if !meta.is_file() || meta.len() != file.size || mtime_ms(meta) != file.mtime_ms {
            return Err(io::Error::other(SourceError::Changed {
                path: file.path.clone(),
            }));
        }
        Ok(())
    }
}

impl SourceReader for FsReader {
    fn read_exact_at(
        &self,
        _index: u32,
        file: &SourceFile,
        offset: u64,
        buf: &mut [u8],
    ) -> io::Result<()> {
        let path = self.root.join(&file.path);
        // Refuse a symlink swapped in after the scan.
        if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Err(io::Error::other(SourceError::Changed {
                path: file.path.clone(),
            }));
        }
        let handle = File::open(&path)?;
        Self::check(file, &handle.metadata()?)?;
        read_exact_at(&handle, offset, buf)?;
        // A write during the read changes the mtime.
        Self::check(file, &handle.metadata()?)
    }
}

#[cfg(unix)]
fn read_exact_at(file: &File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, offset)
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut offset: u64, mut buf: &mut [u8]) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                let rest = std::mem::take(&mut buf);
                buf = rest.get_mut(n..).unwrap_or_default();
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
