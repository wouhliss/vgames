//! Read-only restore preflight. This does not authorize filesystem writes:
//! callers must still acquire the launch lease and use no-follow handles,
//! verified staging blobs, durable backups and cancellation-aware commit.
use std::collections::BTreeSet;
use std::path::Path;

use tokio_util::sync::CancellationToken;
use vgames_core::paths;
use vgames_proto::saves::SaveFile;

use super::scan::{self, Error, Location};

/// Native installed-directory inventory, never a list supplied by the server
/// or WebView. Refuse all writes into installs under security invariant 1;
/// install-based saves need an explicit contract resolution before support.
pub struct ProtectedInstalls {
    roots: BTreeSet<String>,
}
impl ProtectedInstalls {
    pub fn open<'a>(roots: impl IntoIterator<Item = &'a Path>) -> Result<Self, Error> {
        let roots = roots
            .into_iter()
            .map(|root| {
                let root = scan::canonical_base(root)?;
                Ok(paths::fold(
                    &root.to_str().ok_or(Error::Invalid)?.replace('\\', "/"),
                ))
            })
            .map(|root| root.map(|root| root.trim_end_matches('/').to_owned()))
            .collect::<Result<_, Error>>()?;
        Ok(Self { roots })
    }
    fn overlaps(&self, target: &str) -> bool {
        if self.roots.contains(target) {
            return true;
        }
        for (offset, _) in target.match_indices('/') {
            if target
                .get(..offset)
                .is_some_and(|prefix| self.roots.contains(prefix))
            {
                return true;
            }
        }
        let prefix = format!("{target}/");
        self.roots
            .range(prefix.clone()..)
            .next()
            .is_some_and(|root| root.starts_with(&prefix))
    }
}

/// Validate the complete set of files to replace OR delete before staging.
/// Locations must come from the verified manifest and native base resolver.
/// No paths, directories or files are created by this function.
pub fn preflight(
    locations: &[Location],
    files: &[SaveFile],
    protected: &ProtectedInstalls,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let mut roots = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for location in locations {
        if !ids.insert(&location.id) {
            return Err(Error::Invalid);
        }
        // Overlapping locations could bypass a signed filter via another id.
        let previous = ProtectedInstalls {
            roots: roots.clone(),
        };
        if previous.overlaps(&location.path_key) {
            return Err(Error::Invalid);
        }
        roots.insert(location.path_key.clone());
    }
    if files.len() > scan::MAX_FILES {
        return Err(Error::Limit);
    }
    let mut total = 0u64;
    let mut keys = Vec::with_capacity(files.len());
    for file in files {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let location = locations
            .iter()
            .find(|location| location.id == file.root)
            .ok_or(Error::Invalid)?;
        paths::validate_path(&file.path).map_err(|_| Error::Invalid)?;
        if !scan::valid_hash(&file.blake3)
            || (!location.include.is_empty() && !location.include.is_match(&file.path))
            || location.exclude.is_match(&file.path)
        {
            return Err(Error::Invalid);
        }
        // The scanner prunes excluded directories. Apply that same rule to
        // remote files; checking only the final filename is insufficient.
        for (offset, _) in file.path.match_indices('/') {
            if file
                .path
                .get(..offset)
                .is_none_or(|prefix| location.exclude.is_match(prefix))
            {
                return Err(Error::Invalid);
            }
        }
        let target = format!("{}/{}", location.path_key, paths::fold(&file.path));
        if protected.overlaps(&target) {
            return Err(Error::Invalid);
        }
        total = total
            .checked_add(u64::try_from(file.size).map_err(|_| Error::Invalid)?)
            .ok_or(Error::Limit)?;
        if total > scan::MAX_BYTES {
            return Err(Error::Limit);
        }
        keys.push(format!("{}/{}", file.root, file.path));
    }
    paths::validate_tree(keys.iter().map(String::as_str), std::iter::empty())
        .map_err(|_| Error::Invalid)?;
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
