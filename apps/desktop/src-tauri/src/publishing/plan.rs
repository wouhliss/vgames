//! Scanning a folder into what gets uploaded (02-package-format §2, §4): every entry is checked
//! against the path rules before anything leaves the computer.

use std::path::Path;
use std::sync::Arc;

use vgames_core::paths::PathError;
use vgames_pack::plan::PlanError;
use vgames_pack::scan::{FsReader, RejectReason, scan};
use vgames_pack::{PACK_SIZE, PackSource, Plan};

use super::model::{InvalidPath, InvalidPathReason, PublishCommandError, PublishPlan};

/// Invalid entries listed in a plan; the count covers the rest.
pub const MAX_LISTED: usize = 200;

/// File names that run on some platform even without the executable bit.
const RUNNABLE_EXTENSIONS: &[&str] = &["exe", "bat", "cmd", "sh", "x86_64", "appimage"];

/// A scanned folder: the preview, and the source to upload when nothing is invalid.
pub struct Scanned {
    pub plan: PublishPlan,
    pub source: Option<PackSource>,
}

fn invalid(error: &PathError) -> InvalidPath {
    let (reason, other) = match error {
        PathError::NotNfc(_) => (InvalidPathReason::NotNfc, None),
        PathError::BadStructure(_) => (InvalidPathReason::BadStructure, None),
        PathError::TooLong(_) => (InvalidPathReason::TooLong, None),
        PathError::ForbiddenCharacter(_) => (InvalidPathReason::ForbiddenCharacter, None),
        PathError::TrailingDotOrSpace(_) => (InvalidPathReason::TrailingDotOrSpace, None),
        PathError::ReservedName(_) => (InvalidPathReason::ReservedName, None),
        PathError::ReservedRoot(_) => (InvalidPathReason::ReservedFolder, None),
        PathError::UnsafeCompatibilityForm(_) => (InvalidPathReason::UnsafeCompatibilityForm, None),
        PathError::CaseCollision(_, other) => {
            (InvalidPathReason::CaseCollision, Some(other.clone()))
        }
        PathError::FileIsDirectory(_) => (InvalidPathReason::FileIsFolder, None),
        PathError::Duplicate(_) => (InvalidPathReason::Duplicate, None),
    };
    InvalidPath {
        path: error.path().to_owned(),
        reason,
        other,
    }
}

fn runnable(path: &str, executable_bit: bool) -> bool {
    executable_bit
        || path.rsplit_once('.').is_some_and(|(_, ext)| {
            RUNNABLE_EXTENSIONS
                .iter()
                .any(|r| ext.eq_ignore_ascii_case(r))
        })
}

/// Scans `folder` (blocking: run it on a blocking thread).
pub fn scan_folder(folder: &Path) -> Result<Scanned, PublishCommandError> {
    let meta = std::fs::metadata(folder).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => PublishCommandError::NotFound,
        _ => PublishCommandError::io("open the folder", &e),
    })?;
    if !meta.is_dir() {
        return Err(PublishCommandError::NotFound);
    }
    let scanned = scan(folder).map_err(|e| PublishCommandError::io("read the folder", &e))?;
    let mut invalid_paths: Vec<InvalidPath> = scanned
        .rejected
        .iter()
        .map(|r| InvalidPath {
            path: r.path.clone(),
            reason: match r.reason {
                RejectReason::Symlink => InvalidPathReason::Symlink,
                RejectReason::SpecialFile => InvalidPathReason::SpecialFile,
                RejectReason::NotUtf8 => InvalidPathReason::NotUtf8,
            },
            other: None,
        })
        .collect();
    if scanned.files.is_empty() && invalid_paths.is_empty() {
        return Err(PublishCommandError::EmptyFolder);
    }
    let file_count = scanned.files.len() as u64;
    let total_bytes: u64 = scanned.files.iter().map(|f| f.size).sum();
    let mut executables: Vec<String> = scanned
        .files
        .iter()
        .filter(|f| runnable(&f.path, f.executable))
        .map(|f| f.path.clone())
        .collect();
    executables.sort();

    let plan = match Plan::new(scanned.files, scanned.directories) {
        Ok(plan) => Some(plan),
        Err(PlanError::InvalidPaths(errors)) => {
            invalid_paths.extend(errors.iter().map(invalid));
            None
        }
        Err(e) => return Err(PublishCommandError::internal("lay out the files", &e)),
    };
    let invalid_count = invalid_paths.len() as u64;
    invalid_paths.truncate(MAX_LISTED);

    let source = match plan {
        Some(plan) if invalid_count == 0 => {
            let packing = plan
                .raw_packing(PACK_SIZE)
                .map_err(|e| PublishCommandError::internal("lay out the packs", &e))?;
            Some(PackSource::new(
                Arc::new(plan),
                Arc::new(packing),
                Arc::new(FsReader::new(folder)),
            ))
        }
        _ => None,
    };
    let pack_count = source.as_ref().map_or(0, |s| s.packing.pack_count());
    Ok(Scanned {
        plan: PublishPlan {
            folder: folder.to_string_lossy().into_owned(),
            file_count,
            total_bytes,
            pack_count,
            executables,
            invalid_paths,
            invalid_count,
        },
        source,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn a_valid_folder_plans_packs_and_lists_runnable_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::write(dir.path().join("Game.exe"), b"MZ").unwrap();
        std::fs::write(dir.path().join("bin/run.sh"), b"#!/bin/sh").unwrap();
        std::fs::write(dir.path().join("data.pak"), vec![7u8; 4096]).unwrap();
        let scanned = scan_folder(dir.path()).unwrap();
        assert_eq!(scanned.plan.file_count, 3);
        assert_eq!(scanned.plan.total_bytes, 2 + 9 + 4096);
        assert_eq!(scanned.plan.pack_count, 1);
        assert_eq!(scanned.plan.executables, ["Game.exe", "bin/run.sh"]);
        assert_eq!(scanned.plan.invalid_count, 0);
        assert!(scanned.source.is_some());
    }

    #[test]
    fn invalid_entries_are_listed_and_block_the_upload() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ok.txt"), b"x").unwrap();
        std::fs::write(dir.path().join("CON.txt"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("ok.txt", dir.path().join("link")).unwrap();
        let scanned = scan_folder(dir.path()).unwrap();
        assert!(scanned.source.is_none());
        let reasons: Vec<_> = scanned
            .plan
            .invalid_paths
            .iter()
            .map(|p| (p.path.as_str(), p.reason))
            .collect();
        assert!(reasons.contains(&("CON.txt", InvalidPathReason::ReservedName)));
        #[cfg(unix)]
        assert!(reasons.contains(&("link", InvalidPathReason::Symlink)));
        assert_eq!(scanned.plan.invalid_count as usize, reasons.len());
    }

    #[test]
    fn missing_and_empty_folders_are_typed() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            scan_folder(dir.path()),
            Err(PublishCommandError::EmptyFolder)
        ));
        assert!(matches!(
            scan_folder(&dir.path().join("nope")),
            Err(PublishCommandError::NotFound)
        ));
    }
}
