//! Updates and repairs of installed packages (INS-04), through
//! `vgames_transfer::update::execute::{update_safe, repair_safe}` only. The
//! installed release is re-verified under current trust; the new one is
//! fetched with `fetch_release` (G1). While a run works the install is marked
//! `updating` or `repairing`, so the launcher refuses to start it.

use std::path::Path;
use std::sync::Arc;

use uuid::Uuid;
use vgames_core::manifest::Platform as Host;
use vgames_core::verify::{ExpectedRelease, VerifyError, VerifyMode};
use vgames_proto::packages::Platform;
use vgames_proto::versions::ReleaseDescriptor;
use vgames_transfer::download::{DownloadControl, PauseReason as EnginePause};
use vgames_transfer::install::{self, InstallError, InstallOutcome as Outcome, Release};
use vgames_transfer::update::{commit, execute, plan::UpdatePlan, verify};

use super::backend::{Backend, Reporting};
use super::job::{io_error, library_online, verify_failure};
use super::{DownloadError, Downloads, End, PauseReason};
use crate::catalog::CatalogError;
use crate::db;
use crate::db::download_jobs::Job;
use crate::db::installs::InstallRow;
use crate::events::{AppEvent, InstallPhase, InstallProgress, PackageRef};
use crate::libraries::LibraryRoot;

/// What every update and repair run starts from.
struct Installed {
    row: InstallRow,
    library: LibraryRoot,
    platform: Platform,
    expected: ExpectedRelease,
    trust: Arc<vgames_core::trust::TrustState>,
}

fn descriptor_error(error: CatalogError) -> End {
    match error {
        CatalogError::Offline => End::Paused(PauseReason::Offline),
        CatalogError::NotFound => End::Failed(DownloadError::VersionUnavailable),
        CatalogError::Unauthenticated => End::Failed(DownloadError::Server {
            code: "unauthenticated".into(),
            message: "Sign in again to continue".into(),
        }),
        CatalogError::Server { code, message } => {
            End::Failed(DownloadError::Server { code, message })
        }
    }
}

impl<B: Backend> Downloads<B> {
    async fn installed(&self, job: &Job, busy: &'static str) -> Result<Installed, End> {
        let package = job.package;
        let row = match db::installs::row(self.db(), package).await {
            Ok(Some(row)) => row,
            Ok(None) => {
                return Err(End::Failed(DownloadError::Server {
                    code: "not_installed".into(),
                    message: "The package is not installed".into(),
                }));
            }
            Err(error) => return Err(End::Failed(io_error(Path::new(""), error))),
        };
        let library = LibraryRoot {
            id: job.library_id,
            path: row.library_path.clone(),
        };
        if !library_online(&library).await {
            return Err(End::Paused(PauseReason::LibraryOffline {
                library_path: library.path.to_string_lossy().into_owned(),
            }));
        }
        let platform = Platform::parse(&row.platform).ok_or_else(|| {
            End::Failed(DownloadError::Server {
                code: "invalid_platform".into(),
                message: format!("Unknown platform {}", row.platform),
            })
        })?;
        let host_platform = Host::ALL
            .into_iter()
            .find(|p| p.as_str() == platform.as_str())
            .ok_or(End::Failed(DownloadError::SignatureInvalid))?;
        let (Ok(version_id), Ok(sequence)) = (
            Uuid::parse_str(&row.version_id),
            u64::try_from(row.sequence),
        ) else {
            return Err(End::Failed(DownloadError::SignatureInvalid));
        };
        let trust = match self.backend().trust(package.server_id, false).await {
            Ok(Some(trust)) => trust,
            Ok(None) => return Err(End::Failed(DownloadError::UntrustedKey)),
            Err(error) => {
                return Err(End::Failed(DownloadError::Server {
                    code: "trust_unavailable".into(),
                    message: error.to_string(),
                }));
            }
        };
        if let Err(error) = db::installs::set_state(self.db(), package, busy).await {
            return Err(End::Failed(io_error(&row.root, error)));
        }
        // An interrupted commit is finished before anything reads the manifest.
        let root = row.root.clone();
        let recover_trust = Arc::clone(&trust);
        match tokio::task::spawn_blocking(move || commit::recover_pending(&root, &recover_trust))
            .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => return Err(self.fail_back(package, &row.root, error).await),
            Err(error) => return Err(End::Failed(io_error(&row.root, error))),
        }
        Ok(Installed {
            expected: ExpectedRelease {
                server_id: package.server_id,
                package_id: package.package_id,
                version_id,
                platform: host_platform,
                sequence,
            },
            row,
            library,
            platform,
            trust,
        })
    }

    async fn fail_back(&self, package: PackageRef, root: &Path, error: InstallError) -> End {
        self.back_to_installed(package).await;
        End::Failed(verify_failure(&error).unwrap_or_else(|| io_error(root, error)))
    }

    /// The install is playable as it was (paused, failed or cancelled run).
    async fn back_to_installed(&self, package: PackageRef) {
        if let Err(error) = db::installs::set_state(self.db(), package, "installed").await {
            tracing::error!(%error, "cannot mark the install installed again");
        }
    }

    /// The installed release, re-verified under current trust (launch rules:
    /// an expired bundle does not matter, a revoked key does).
    async fn local_release(&self, installed: &Installed) -> Result<Release, InstallError> {
        let root = installed.row.root.clone();
        let trust = Arc::clone(&installed.trust);
        let expected = installed.expected.clone();
        tokio::task::spawn_blocking(move || {
            install::load_local_release(&root, &trust, &expected, VerifyMode::Launch)
        })
        .await
        .map_err(|e| InstallError::Internal(e.to_string()))?
    }

    async fn ended(&self, job: &Job, end: End) -> End {
        if !matches!(end, End::Installed) {
            self.back_to_installed(job.package).await;
        }
        end
    }

    pub(crate) async fn update(&self, job: &Job, control: &DownloadControl) -> End {
        let installed = match self.installed(job, "updating").await {
            Ok(installed) => installed,
            Err(end) => return self.ended(job, end).await,
        };
        let end = self.run_update(job, control, &installed).await;
        self.ended(job, end).await
    }

    async fn run_update(&self, job: &Job, control: &DownloadControl, installed: &Installed) -> End {
        let package = job.package;
        let root = &installed.row.root;
        let descriptor = match self.backend().descriptor(package, installed.platform).await {
            Ok(descriptor) => descriptor,
            Err(error) => return descriptor_error(error),
        };
        if descriptor.version_id == installed.expected.version_id {
            return End::Installed;
        }
        let old = match self.local_release(installed).await {
            Ok(old) => Arc::new(old),
            Err(error) => {
                return End::Failed(
                    verify_failure(&error).unwrap_or_else(|| io_error(root, error)),
                );
            }
        };
        let yanked = descriptor
            .yanked_version_ids
            .contains(&installed.expected.version_id);
        let Ok(sequence) = u64::try_from(descriptor.sequence) else {
            return End::Failed(DownloadError::SignatureInvalid);
        };
        let expected = ExpectedRelease {
            version_id: descriptor.version_id,
            sequence,
            ..installed.expected.clone()
        };
        let new = match self
            .fetch(
                &descriptor,
                &expected,
                Some(installed.expected.sequence),
                yanked,
            )
            .await
        {
            Ok(new) => Arc::new(new),
            Err(end) => return end,
        };
        let plan = match UpdatePlan::new(&old, &new, yanked) {
            Ok(plan) => plan,
            Err(error) => return End::Failed(io_error(root, error)),
        };
        let required = plan
            .safe_extra_bytes
            .saturating_add(vgames_transfer::install::SPACE_MARGIN);
        let available = self.free_space(&installed.library.path);
        if available < required {
            return End::Paused(PauseReason::DiskFull {
                library_path: installed.library.path.to_string_lossy().into_owned(),
                required_bytes: required,
                available_bytes: available,
            });
        }
        let packs = match self.backend().packs(package, descriptor.version_id).await {
            Ok(packs) => Arc::new(Reporting::new(packs)),
            Err(CatalogError::Offline) => return End::Paused(PauseReason::Offline),
            Err(error) => return descriptor_error(error),
        };
        let result = execute::update_safe(
            root,
            Arc::clone(&old),
            Arc::clone(&new),
            Arc::clone(&packs),
            &self.options(),
            control,
            yanked,
        )
        .await;
        let end = self
            .transfer_end(job, control, installed, result, packs.reported(), &new)
            .await;
        if end == End::Installed {
            self.adopted(job, installed, &descriptor, &new).await
        } else {
            end
        }
    }

    /// The new release is installed: record it and drop cached state.
    async fn adopted(
        &self,
        job: &Job,
        installed: &Installed,
        descriptor: &ReleaseDescriptor,
        new: &Release,
    ) -> End {
        let size = new.manifest().files.iter().map(|f| f.size).sum();
        let db = self.db();
        let recorded = async {
            db::installs::set_installed(
                db,
                job.package,
                descriptor.version_id,
                descriptor.sequence,
                size,
            )
            .await?;
            db::installs::set_version_label(db, job.package, descriptor.version_label.clone()).await
        }
        .await;
        if let Err(error) = recorded {
            return End::Failed(io_error(&installed.row.root, error));
        }
        self.files_changed(job.package, &installed.row.root);
        End::Installed
    }

    async fn transfer_end(
        &self,
        job: &Job,
        control: &DownloadControl,
        installed: &Installed,
        result: Result<install::InstallReport, InstallError>,
        reported: bool,
        release: &Arc<Release>,
    ) -> End {
        match result {
            Ok(report) => {
                self.count_verified(report.stats.chunks_verified);
                match report.outcome {
                    Outcome::Installed(_) | Outcome::PriorityFilesReady { .. } => End::Installed,
                    Outcome::Paused(EnginePause::User) => End::Paused(PauseReason::User),
                    Outcome::Paused(EnginePause::DiskFull) => End::Paused(PauseReason::DiskFull {
                        library_path: installed.library.path.to_string_lossy().into_owned(),
                        required_bytes: 0,
                        available_bytes: self.free_space(&installed.library.path),
                    }),
                    Outcome::Cancelled => self.cancelled(job, &installed.row.root, release).await,
                }
            }
            Err(error) => {
                let _ = control;
                self.install_error(&installed.library, &installed.row.root, error, reported)
                    .await
            }
        }
    }

    pub(crate) async fn repair(&self, job: &Job, control: &DownloadControl) -> End {
        let installed = match self.installed(job, "repairing").await {
            Ok(installed) => installed,
            Err(end) => return self.ended(job, end).await,
        };
        let end = self.run_repair(job, control, &installed).await;
        if end == End::Installed {
            self.back_to_installed(job.package).await;
            self.files_changed(job.package, &installed.row.root);
        }
        self.ended(job, end).await
    }

    async fn run_repair(&self, job: &Job, control: &DownloadControl, installed: &Installed) -> End {
        let package = job.package;
        let root = &installed.row.root;
        let release = match self.local_release(installed).await {
            Ok(release) => Arc::new(release),
            Err(InstallError::Verify(
                VerifyError::RevokedKey(_)
                | VerifyError::UnknownKey(_)
                | VerifyError::KeyNotValidNow(_),
            )) => match self.adopt_resigned(installed).await {
                Ok(release) => Arc::new(release),
                Err(end) => return end,
            },
            Err(error) => {
                return End::Failed(
                    verify_failure(&error).unwrap_or_else(|| io_error(root, error)),
                );
            }
        };
        self.bus()
            .publish(AppEvent::InstallProgress(InstallProgress {
                package,
                phase: InstallPhase::Verifying,
                bytes_done: 0,
                bytes_total: release.manifest().files.iter().map(|f| f.size).sum(),
                bytes_per_second: 0,
                eta_seconds: None,
                connections: 0,
            }));
        let report = match verify::verify_install(
            root,
            Arc::clone(&release),
            verify::VerifyOptions::default(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        {
            Ok(report) => report,
            Err(error) => {
                if !library_online(&installed.library).await {
                    return End::Paused(PauseReason::LibraryOffline {
                        library_path: installed.library.path.to_string_lossy().into_owned(),
                    });
                }
                return End::Failed(io_error(root, error));
            }
        };
        if report.damaged_files.is_empty() {
            return End::Installed;
        }
        tracing::warn!(%package.package_id, damaged = report.damaged_files.len(), "repairing damaged files");
        let packs = match self
            .backend()
            .packs(package, installed.expected.version_id)
            .await
        {
            Ok(packs) => Arc::new(Reporting::new(packs)),
            Err(CatalogError::Offline) => return End::Paused(PauseReason::Offline),
            Err(error) => return descriptor_error(error),
        };
        let result = execute::repair_safe(
            root,
            Arc::clone(&release),
            &report.damaged_files,
            Arc::clone(&packs),
            &self.options(),
            control,
        )
        .await;
        self.transfer_end(job, control, installed, result, packs.reported(), &release)
            .await
    }

    /// Security review F1: the installed envelope no longer verifies (its
    /// key was revoked or rotated). If the server still offers this exact
    /// version and its current signature verifies under the current bundle
    /// for the same manifest bytes, that signature replaces
    /// `.vgames/manifest.sig`; nothing is downloaded again.
    async fn adopt_resigned(&self, installed: &Installed) -> Result<Release, End> {
        let package = PackageRef {
            server_id: installed.expected.server_id,
            package_id: installed.expected.package_id,
        };
        let descriptor = self
            .backend()
            .descriptor(package, installed.platform)
            .await
            .map_err(descriptor_error)?;
        if descriptor.version_id != installed.expected.version_id {
            // The installed version is not offered any more: update instead.
            return Err(End::Failed(DownloadError::UntrustedKey));
        }
        let envelope = serde_json::to_vec(&descriptor.signature)
            .ok()
            .and_then(|bytes| vgames_core::Envelope::parse(&bytes).ok())
            .ok_or(End::Failed(DownloadError::SignatureInvalid))?;
        let root = installed.row.root.clone();
        let trust = Arc::clone(&installed.trust);
        let expected = installed.expected.clone();
        let adopted = tokio::task::spawn_blocking(move || -> Result<Release, InstallError> {
            // Same bytes, new signature, current trust.
            install::adopt_signature(&root, &trust, envelope, &expected)
        })
        .await
        .map_err(|e| End::Failed(io_error(&installed.row.root, e)))?;
        match adopted {
            Ok(release) => {
                tracing::info!(%package.package_id, "adopted the re-signed manifest signature");
                self.files_changed(package, &installed.row.root);
                Ok(release)
            }
            Err(error) => Err(End::Failed(
                verify_failure(&error).unwrap_or_else(|| io_error(&installed.row.root, error)),
            )),
        }
    }
}
