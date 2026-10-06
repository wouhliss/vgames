//! One run of a queued job, from the release descriptor to the installed
//! files, through `vgames_transfer::install::{fetch_release, install}` only.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use vgames_core::Timestamp;
use vgames_core::manifest::Platform as Host;
use vgames_core::verify::{ExpectedRelease, VerifyError, VerifyMode};
use vgames_proto::packages::Platform;
use vgames_transfer::download::manifest::{ManifestFetchError, ManifestLink};
use vgames_transfer::download::{
    DownloadControl, DownloadError as EngineError, PauseReason as EnginePause, Phase,
};
use vgames_transfer::install::{self, InstallError, InstallOutcome as Outcome, Release};

use super::backend::{Backend, Reporting};
use super::{DownloadError, Downloads, End, PauseReason, Stop};
use crate::catalog::CatalogError;
use crate::db;
use crate::db::download_jobs::{Job, JobKind};
use crate::events::{AppEvent, InstallPhase, InstallProgress};
use crate::libraries::{self, LibraryPresence, LibraryRoot};

/// Longest wait for the next manifest byte.
const MANIFEST_STALL: Duration = Duration::from_secs(20);

fn io_error(path: &Path, detail: impl std::fmt::Display) -> DownloadError {
    DownloadError::Io {
        path: path.to_string_lossy().into_owned(),
        detail: detail.to_string(),
    }
}

impl<B: Backend> Downloads<B> {
    pub(crate) async fn run(&self, job: &Job, control: &DownloadControl) -> End {
        let forward = self.forward_progress(job, control);
        let end = match job.kind {
            JobKind::Install => self.install(job, control).await,
            // INS-04 runs updates and repairs through `update::execute`.
            JobKind::Update | JobKind::Repair => End::Failed(DownloadError::Server {
                code: "unsupported".into(),
                message: "Updates and repairs are not available yet".into(),
            }),
        };
        forward.cancel();
        end
    }

    /// Publishes `install-progress` (the engine sends at most 4 per second)
    /// until the run ends.
    fn forward_progress(
        &self,
        job: &Job,
        control: &DownloadControl,
    ) -> tokio_util::sync::CancellationToken {
        let done = tokio_util::sync::CancellationToken::new();
        let stop = done.clone();
        let mut progress = control.progress();
        let bus = self.bus().clone();
        let package = job.package;
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::select! {
                    () = stop.cancelled() => return,
                    changed = progress.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        let p = progress.borrow_and_update().clone();
                        bus.publish(AppEvent::InstallProgress(InstallProgress {
                            package,
                            phase: phase(p.phase),
                            bytes_done: p.bytes_done,
                            bytes_total: p.bytes_total,
                            bytes_per_second: p.bytes_per_second,
                            eta_seconds: p.eta_seconds,
                            connections: p.connections,
                        }));
                    }
                }
            }
        });
        done
    }

    async fn install(&self, job: &Job, control: &DownloadControl) -> End {
        let package = job.package;
        let db = self.db();
        let row = match db::installs::row(db, package).await {
            Ok(Some(row)) => row,
            Ok(None) => {
                return End::Failed(DownloadError::Server {
                    code: "not_installed".into(),
                    message: "The install record is missing".into(),
                });
            }
            Err(error) => return End::Failed(io_error(Path::new(""), error)),
        };
        let library = LibraryRoot {
            id: job.library_id,
            path: row.library_path.clone(),
        };
        if !library_online(&library).await {
            return End::Paused(PauseReason::LibraryOffline {
                library_path: library.path.to_string_lossy().into_owned(),
            });
        }
        let Some(platform) = Platform::parse(&row.platform) else {
            return End::Failed(DownloadError::Server {
                code: "invalid_platform".into(),
                message: format!("Unknown platform {}", row.platform),
            });
        };
        control.set_phase(Phase::VerifyingManifest);
        let descriptor = match self.backend().descriptor(package, platform).await {
            Ok(descriptor) => descriptor,
            Err(CatalogError::Offline) => return End::Paused(PauseReason::Offline),
            Err(CatalogError::NotFound) => return End::Failed(DownloadError::VersionUnavailable),
            Err(CatalogError::Unauthenticated) => {
                return End::Failed(DownloadError::Server {
                    code: "unauthenticated".into(),
                    message: "Sign in again to continue".into(),
                });
            }
            Err(CatalogError::Server { code, message }) => {
                return End::Failed(DownloadError::Server { code, message });
            }
        };
        // A partial install of another version cannot continue (the journal
        // describes other bytes): the player cancels it and installs again.
        let root = row.root.clone();
        let partial = {
            let root = root.clone();
            tokio::task::spawn_blocking(move || install::read_record(&root))
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
        };
        if partial.is_some_and(|record| record.version_id != descriptor.version_id) {
            return End::Failed(DownloadError::VersionUnavailable);
        }
        let Ok(sequence) = u64::try_from(descriptor.sequence) else {
            return End::Failed(DownloadError::SignatureInvalid);
        };
        let Some(host_platform) = Host::ALL
            .into_iter()
            .find(|p| p.as_str() == platform.as_str())
        else {
            return End::Failed(DownloadError::SignatureInvalid);
        };
        let expected = ExpectedRelease {
            server_id: package.server_id,
            package_id: package.package_id,
            version_id: descriptor.version_id,
            platform: host_platform,
            sequence,
        };
        let release = match self.fetch(&descriptor, &expected).await {
            Ok(release) => Arc::new(release),
            Err(end) => return end,
        };
        let packs = match self.backend().packs(package, descriptor.version_id).await {
            Ok(packs) => Arc::new(Reporting::new(packs)),
            Err(CatalogError::Offline) => return End::Paused(PauseReason::Offline),
            Err(error) => {
                return End::Failed(DownloadError::Server {
                    code: "unavailable".into(),
                    message: error.to_string(),
                });
            }
        };
        if let Some(end) = self.space_check(&library, &root, &release).await {
            return end;
        }

        let hook = self.hook();
        let mut options = self.options();
        options.priority_files = hook.priority_files(package, release.manifest());
        loop {
            let result = install::install(
                &root,
                Arc::clone(&release),
                Arc::clone(&packs),
                &options,
                control,
            )
            .await;
            let outcome = match result {
                Ok(report) => {
                    self.count_verified(report.stats.chunks_verified);
                    report.outcome
                }
                Err(error) => {
                    return self
                        .install_error(&library, &root, error, packs.reported())
                        .await;
                }
            };
            match outcome {
                Outcome::Installed(_) => break,
                Outcome::PriorityFilesReady { paths } => {
                    if let Err(blocker) = hook.check(package, &paths).await {
                        tracing::warn!(%package.package_id, ?blocker, "the launch target was refused");
                        let root = root.clone();
                        let release = Arc::clone(&release);
                        let removed = tokio::task::spawn_blocking(move || {
                            install::remove_install(&root, release.manifest())?;
                            remove_if_empty(&root);
                            Ok::<_, InstallError>(())
                        })
                        .await;
                        if !matches!(removed, Ok(Ok(()))) {
                            tracing::warn!(?removed, "cannot remove a refused install's files");
                        }
                        if let Err(error) = db::installs::delete(db, package).await {
                            tracing::error!(%error, "cannot forget a refused install");
                        }
                        return End::Failed(DownloadError::Blocked { blocker });
                    }
                    options.priority_files.clear();
                }
                Outcome::Paused(EnginePause::User) => return End::Paused(PauseReason::User),
                Outcome::Paused(EnginePause::DiskFull) => {
                    return self.space_check(&library, &root, &release).await.unwrap_or(
                        End::Paused(PauseReason::DiskFull {
                            library_path: library.path.to_string_lossy().into_owned(),
                            required_bytes: 0,
                            available_bytes: 0,
                        }),
                    );
                }
                Outcome::Cancelled => return self.cancelled(job, &root, &release).await,
            }
        }
        let size = release.manifest().files.iter().map(|f| f.size).sum();
        if let Err(error) = db::installs::set_installed(
            db,
            package,
            descriptor.version_id,
            descriptor.sequence,
            size,
        )
        .await
        {
            return End::Failed(io_error(&root, error));
        }
        End::Installed
    }

    /// Manifest bytes (size + BLAKE3) → `verify_manifest`. An unknown signing
    /// key refreshes the trust bundle once and tries again.
    async fn fetch(
        &self,
        descriptor: &vgames_proto::versions::ReleaseDescriptor,
        expected: &ExpectedRelease,
    ) -> Result<Release, End> {
        let link = ManifestLink::try_from(&descriptor.manifest)
            .map_err(|_| End::Failed(DownloadError::SignatureInvalid))?;
        let envelope = serde_json::to_vec(&descriptor.signature)
            .ok()
            .and_then(|bytes| vgames_core::Envelope::parse(&bytes).ok())
            .ok_or(End::Failed(DownloadError::SignatureInvalid))?;
        let mut refreshed = false;
        loop {
            let trust = match self.backend().trust(expected.server_id, refreshed).await {
                Ok(Some(trust)) => trust,
                Ok(None) => return Err(End::Failed(DownloadError::UntrustedKey)),
                Err(error) => {
                    return Err(End::Failed(DownloadError::Server {
                        code: "trust_unavailable".into(),
                        message: error.to_string(),
                    }));
                }
            };
            let mode = VerifyMode::Install {
                now: Timestamp::new(time::OffsetDateTime::now_utc()),
                allow_older: false,
            };
            let result = install::fetch_release(
                self.backend().transfer_http(),
                &link,
                envelope.clone(),
                &trust,
                expected,
                None,
                mode,
                MANIFEST_STALL,
            )
            .await;
            match result {
                Ok(release) => return Ok(release),
                Err(InstallError::Verify(VerifyError::UnknownKey(_))) if !refreshed => {
                    refreshed = true;
                }
                Err(error) => {
                    if let Some(failure) = verify_failure(&error) {
                        return Err(End::Failed(failure));
                    }
                    return Err(match error {
                        // Includes an expired link: the next run gets a fresh one.
                        InstallError::Manifest(
                            ManifestFetchError::Network(_) | ManifestFetchError::Status(_),
                        ) => End::Paused(PauseReason::Offline),
                        other => End::Failed(DownloadError::Server {
                            code: "manifest".into(),
                            message: other.to_string(),
                        }),
                    });
                }
            }
        }
    }

    async fn install_error(
        &self,
        library: &LibraryRoot,
        root: &Path,
        error: InstallError,
        reported: bool,
    ) -> End {
        if let Some(failure) = verify_failure(&error) {
            return End::Failed(failure);
        }
        match error {
            InstallError::Download(EngineError::Integrity { .. }) => {
                End::Failed(DownloadError::DamagedFile { reported })
            }
            InstallError::Download(EngineError::Network(_) | EngineError::LinksRefused { .. }) => {
                End::Paused(PauseReason::Offline)
            }
            InstallError::Download(EngineError::Remote(remote)) => match remote.code.as_deref() {
                Some("version_yanked" | "not_found" | "version_unavailable") => {
                    End::Failed(DownloadError::VersionUnavailable)
                }
                _ if remote.retryable => End::Paused(PauseReason::Offline),
                code => End::Failed(DownloadError::Server {
                    code: code.unwrap_or("refused").to_owned(),
                    message: remote.message,
                }),
            },
            InstallError::NotEnoughSpace {
                required,
                available,
            } => End::Paused(PauseReason::DiskFull {
                library_path: library.path.to_string_lossy().into_owned(),
                required_bytes: required,
                available_bytes: available,
            }),
            InstallError::Cancelled => End::Cancelled { keep_partial: true },
            other => {
                // A drive that went away mid-install is "library offline".
                if !library_online(library).await {
                    return End::Paused(PauseReason::LibraryOffline {
                        library_path: library.path.to_string_lossy().into_owned(),
                    });
                }
                End::Failed(io_error(root, other))
            }
        }
    }

    /// `None` when the install fits; otherwise the pause (or failure) to record.
    async fn space_check(
        &self,
        library: &LibraryRoot,
        root: &Path,
        release: &Arc<Release>,
    ) -> Option<End> {
        let disk_full = |required, available| {
            Some(End::Paused(PauseReason::DiskFull {
                library_path: library.path.to_string_lossy().into_owned(),
                required_bytes: required,
                available_bytes: available,
            }))
        };
        let root_owned = root.to_owned();
        let release = Arc::clone(release);
        let checked = tokio::task::spawn_blocking(move || {
            install::check_space(&root_owned, release.manifest())
        })
        .await;
        match checked {
            Ok(Ok(space)) => {
                let available = self.free_space(&library.path);
                (available < space.required).then_some(())?;
                disk_full(space.required, available)
            }
            Ok(Err(InstallError::NotEnoughSpace {
                required,
                available,
            })) => disk_full(required, available),
            Ok(Err(error)) => Some(End::Failed(io_error(&library.path, error))),
            Err(error) => Some(End::Failed(io_error(&library.path, error))),
        }
    }

    /// Whether a library-offline or disk-full pause has cleared.
    pub(crate) async fn condition_cleared(&self, job: &Job, reason: &PauseReason) -> bool {
        let Ok(Some(row)) = db::installs::row(self.db(), job.package).await else {
            return false;
        };
        let library = LibraryRoot {
            id: job.library_id,
            path: row.library_path,
        };
        if !library_online(&library).await {
            return false;
        }
        match reason {
            PauseReason::LibraryOffline { .. } => true,
            PauseReason::DiskFull { required_bytes, .. } => {
                self.free_space(&library.path) >= *required_bytes
            }
            PauseReason::User | PauseReason::Offline => false,
        }
    }

    async fn cancelled(&self, job: &Job, root: &Path, release: &Arc<Release>) -> End {
        let keep_partial = match self.stop_requested(job.package) {
            Some(Stop::Cancel { keep_partial }) => keep_partial,
            _ => true,
        };
        if !keep_partial {
            let root = root.to_owned();
            let release = Arc::clone(release);
            let removed = tokio::task::spawn_blocking(move || {
                install::remove_install(&root, release.manifest())?;
                remove_if_empty(&root);
                Ok::<_, InstallError>(())
            })
            .await;
            if !matches!(removed, Ok(Ok(()))) {
                tracing::warn!(?removed, "cannot remove a cancelled install's files");
            }
            if let Err(error) = db::installs::delete(self.db(), job.package).await {
                tracing::error!(%error, "cannot forget a cancelled install");
            }
        }
        End::Cancelled { keep_partial }
    }
}

/// Signature and trust failures: never retried automatically, nothing written.
fn verify_failure(error: &InstallError) -> Option<DownloadError> {
    Some(match error {
        InstallError::Verify(VerifyError::TrustExpired) => DownloadError::TrustExpired,
        InstallError::Verify(
            VerifyError::UnknownKey(_)
            | VerifyError::RevokedKey(_)
            | VerifyError::KeyNotValidNow(_)
            | VerifyError::NotKeyHolder(_)
            | VerifyError::TrustServerMismatch,
        ) => DownloadError::UntrustedKey,
        InstallError::Verify(_)
        | InstallError::Layout(_)
        | InstallError::Manifest(
            ManifestFetchError::Hash | ManifestFetchError::Size | ManifestFetchError::TooLarge(_),
        ) => DownloadError::SignatureInvalid,
        _ => return None,
    })
}

async fn library_online(library: &LibraryRoot) -> bool {
    let library = library.clone();
    matches!(
        tokio::task::spawn_blocking(move || libraries::inspect_root(&library)).await,
        Ok(Ok(LibraryPresence::Online { .. }))
    )
}

fn phase(phase: Phase) -> InstallPhase {
    match phase {
        Phase::Queued => InstallPhase::Queued,
        Phase::VerifyingManifest => InstallPhase::VerifyingManifest,
        Phase::Allocating => InstallPhase::Allocating,
        Phase::Downloading => InstallPhase::Downloading,
        Phase::Finalizing | Phase::Done => InstallPhase::Finalizing,
        Phase::Paused => InstallPhase::Paused,
    }
}

fn remove_if_empty(root: &Path) {
    // Fails harmlessly when the player put files there.
    let _ = std::fs::remove_dir(root);
}

/// Deletes an unfinished install's files using its stored manifest (listed
/// paths only, no links followed), then the folder if it is empty.
pub(crate) fn discard_partial(root: &Path) -> Result<(), InstallError> {
    let manifest_path = root.join(install::META_DIR).join(install::MANIFEST_FILE);
    match std::fs::read(&manifest_path) {
        Ok(bytes) => {
            let manifest = vgames_core::manifest::parse_and_validate(&bytes).map_err(|e| {
                InstallError::Conflict(format!("the stored manifest is unreadable: {e}"))
            })?;
            install::remove_install(root, &manifest)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(InstallError::Io {
                op: "read",
                path: manifest_path,
                source: error,
            });
        }
    }
    remove_if_empty(root);
    Ok(())
}
