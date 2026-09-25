//! The upload resume file (02 §6): which version is being uploaded and, per
//! pack, its session URI and the offset the storage server confirmed. It
//! holds signed session URIs (credentials), so it is written `0600` and
//! never logged. A resume file only applies to the exact same plan: the plan
//! digest covers every path, size, mtime, executable bit and chunk encoding.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vgames_pack::{Packing, Plan};

use crate::fsutil;

pub const FORMAT: &str = "vgames.upload-resume/1";
const MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackState {
    /// Resumable session URI (a credential).
    pub session: Option<String>,
    /// Bytes the server confirmed.
    pub confirmed: u64,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeState {
    pub format: String,
    pub server_id: Uuid,
    pub package_id: Uuid,
    pub version_id: Uuid,
    pub sequence: u64,
    pub version_label: String,
    pub platform: String,
    pub plan_digest: String,
    pub packs: BTreeMap<u32, PackState>,
}

/// BLAKE3 over the plan and packing: what makes pack bytes identical.
pub fn plan_digest(plan: &Plan, packing: &Packing) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"vgames/upload-plan/v1\0");
    for file in plan.files() {
        h.update(&(file.path.len() as u64).to_le_bytes());
        h.update(file.path.as_bytes());
        h.update(&file.size.to_le_bytes());
        h.update(&file.mtime_ms.to_le_bytes());
        h.update(&[u8::from(file.executable)]);
    }
    for dir in plan.directories() {
        h.update(&(dir.len() as u64).to_le_bytes());
        h.update(dir.as_bytes());
    }
    for chunk in &packing.chunks {
        h.update(&[chunk.encoding as u8]);
        h.update(&chunk.stored_size.to_le_bytes());
    }
    h.finalize().to_hex().to_string()
}

/// The resume file on disk.
#[derive(Debug)]
pub struct ResumeFile {
    path: PathBuf,
}

impl ResumeFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The saved state, or `None` when there is none or it is unreadable.
    pub fn load(&self) -> Option<ResumeState> {
        let meta = std::fs::symlink_metadata(&self.path).ok()?;
        if !meta.is_file() || meta.len() > MAX_BYTES {
            tracing::warn!(path = %self.path.display(), "ignoring an unusable upload resume file");
            return None;
        }
        let bytes = std::fs::read(&self.path).ok()?;
        match serde_json::from_slice::<ResumeState>(&bytes) {
            Ok(state) if state.format == FORMAT => Some(state),
            _ => {
                tracing::warn!(path = %self.path.display(), "ignoring an unreadable upload resume file");
                None
            }
        }
    }

    /// Saves atomically, readable by the owner only.
    pub fn save(&self, state: &ResumeState) -> io::Result<()> {
        if let Some(dir) = self.path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec(state).map_err(io::Error::other)?;
        let mut tmp_name = self.path.file_name().unwrap_or_default().to_owned();
        tmp_name.push(".tmp");
        let tmp = self.path.with_file_name(tmp_name);
        {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        if let Some(dir) = self.path.parent()
            && !dir.as_os_str().is_empty()
        {
            fsutil::sync_dir(dir)?;
        }
        Ok(())
    }

    pub fn remove(&self) -> io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use vgames_pack::SourceFile;

    use super::*;

    fn plan(mtime: i64) -> (Plan, Packing) {
        let plan = Plan::new(
            vec![SourceFile {
                path: "a/b.bin".into(),
                size: 10,
                mtime_ms: mtime,
                executable: false,
            }],
            vec![],
        )
        .unwrap();
        let packing = plan.raw_packing(vgames_pack::PACK_SIZE).unwrap();
        (plan, packing)
    }

    #[test]
    fn digest_tracks_the_tree() {
        let (a, pa) = plan(1);
        let (b, pb) = plan(2);
        assert_eq!(plan_digest(&a, &pa), plan_digest(&a, &pa));
        assert_ne!(plan_digest(&a, &pa), plan_digest(&b, &pb));
    }

    #[test]
    fn saves_privately_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let file = ResumeFile::new(dir.path().join("up/resume.json"));
        assert!(file.load().is_none());
        let state = ResumeState {
            format: FORMAT.into(),
            server_id: Uuid::from_u128(1),
            package_id: Uuid::from_u128(2),
            version_id: Uuid::from_u128(3),
            sequence: 4,
            version_label: "1.0".into(),
            platform: "linux-x86_64".into(),
            plan_digest: "x".into(),
            packs: BTreeMap::from([(
                0,
                PackState {
                    session: Some("https://example/session".into()),
                    confirmed: 16,
                    complete: false,
                },
            )]),
        };
        file.save(&state).unwrap();
        assert_eq!(file.load().unwrap(), state);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(file.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::write(file.path(), b"{nope").unwrap();
        assert!(file.load().is_none());
        file.remove().unwrap();
        file.remove().unwrap();
    }
}
