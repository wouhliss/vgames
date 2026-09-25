//! Private, atomic resume records. Session URIs are bearer credentials.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vgames_pack::PackSource;

use super::UploadError;

const MAX_RECORD: u64 = 32 * 1024 * 1024;

// Deliberately no Debug: a session URI authorizes uploads without a token.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PackResume {
    pub session: String,
    pub offset: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    format: String,
    version_id: Uuid,
    source_blake3: String,
    packs: Vec<Option<PackResume>>,
}

pub(super) struct ResumeStore {
    path: PathBuf,
    record: Mutex<Record>,
    // OS lock survives atomic replacement of the record; released on drop/crash.
    _lock: File,
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
}

fn plain_file(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
            Err(io::Error::other("resume path is not a regular file"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn fingerprint(source: &PackSource) -> Result<String, UploadError> {
    // Binds a resumed session to the original scan and encoding, including
    // empty files and directories. The pack reader checks size/mtime again.
    let mut hash = blake3::Hasher::new();
    hash.update(
        &serde_json::to_vec(&(source.plan.files(), source.plan.directories()))
            .map_err(|_| UploadError::Resume("cannot encode source identity"))?,
    );
    for chunk in &source.packing.chunks {
        hash.update(&[match chunk.encoding {
            vgames_pack::encode::Encoding::Raw => 0,
            vgames_pack::encode::Encoding::Zstd => 1,
        }]);
        hash.update(&chunk.stored_size.to_le_bytes());
        hash.update(&chunk.placement.pack.to_le_bytes());
        hash.update(&chunk.placement.offset.to_le_bytes());
    }
    Ok(hash.finalize().to_hex().to_string())
}

impl ResumeStore {
    pub fn open(path: PathBuf, version_id: Uuid, source: &PackSource) -> Result<Self, UploadError> {
        let identity = fingerprint(source)?;
        let lock_path = path.with_extension("upload-lock");
        plain_file(&lock_path)?;
        let lock = private_options()
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        lock.try_lock()
            .map_err(|_| UploadError::Resume("another upload is using this resume record"))?;
        plain_file(&path)?;
        let record = match private_options().open(&path) {
            Ok(file) => {
                if file.metadata()?.len() > MAX_RECORD {
                    return Err(UploadError::Resume("resume record is too large"));
                }
                let mut bytes = Vec::new();
                file.take(MAX_RECORD + 1).read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_RECORD {
                    return Err(UploadError::Resume("resume record is too large"));
                }
                // Do not expose serde errors: they may quote a session URI.
                let record: Record = serde_json::from_slice(&bytes)
                    .map_err(|_| UploadError::Resume("invalid resume record"))?;
                if record.format != "vgames.upload/1"
                    || record.version_id != version_id
                    || record.source_blake3 != identity
                    || record.packs.len() != source.packing.packs.len()
                {
                    return Err(UploadError::Resume(
                        "source or version changed; start the upload again",
                    ));
                }
                for (pack, size) in record.packs.iter().zip(&source.packing.packs) {
                    if pack.as_ref().is_some_and(|p| p.offset > size.size) {
                        return Err(UploadError::Resume("resume offset exceeds pack size"));
                    }
                }
                record
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Record {
                format: "vgames.upload/1".into(),
                version_id,
                source_blake3: identity,
                packs: vec![None; source.packing.packs.len()],
            },
            Err(error) => return Err(error.into()),
        };
        write_private(&path, &record)?;
        Ok(Self {
            path,
            record: Mutex::new(record),
            _lock: lock,
        })
    }

    pub fn get(&self, pack: u32) -> Result<Option<PackResume>, UploadError> {
        Ok(self
            .record
            .lock()
            .map_err(|_| UploadError::Internal)?
            .packs
            .get(pack as usize)
            .cloned()
            .flatten())
    }

    pub async fn save(self: &Arc<Self>, pack: u32, value: PackResume) -> Result<(), UploadError> {
        let this = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let mut record = this.record.lock().map_err(|_| UploadError::Internal)?;
            *record
                .packs
                .get_mut(pack as usize)
                .ok_or(UploadError::Internal)? = Some(value);
            write_private(&this.path, &record)?;
            Ok(())
        })
        .await
        .map_err(|_| UploadError::Internal)?
    }
}

fn write_private(path: &Path, record: &Record) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = parent.join(format!(".vgames-upload-{}.tmp", Uuid::now_v7()));
    let result = (|| {
        let mut file = private_options().create_new(true).open(&temporary)?;
        let bytes = serde_json::to_vec(record)
            .map_err(|_| io::Error::other("cannot encode resume record"))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        crate::fsutil::sync_dir(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}
