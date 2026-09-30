//! Install queue worker (A2-T08): runs one job at a time from the durable
//! `download_jobs` table and maps how each run ended to a durable state and an
//! [`InstallFinished`] event. The transfer itself is behind [`JobRunner`], so
//! the queue rules are tested without a network.
//!
//! Event-driven: the worker sleeps on a `Notify` and the shutdown token; there
//! are no timers. Progress is forwarded from the engine's `watch` channel, which
//! already limits it to 4 Hz.

use std::future::Future;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_transfer::download::{DownloadControl, PauseReason, Phase};

use crate::db::Db;
use crate::db::download_jobs::{self, Job, JobKind, JobOptions, JobStoreError, JobTransition};
use crate::events::{
    AppEvent, EventBus, InstallFinished, InstallOutcome, InstallPhase, InstallProgress, PackageRef,
};

/// How one run of a job ended, as reported by the runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunResult {
    Installed,
    Paused(PauseReason),
    Cancelled,
    /// `code` is a stable machine-readable id shown to the UI.
    Failed {
        code: String,
        message: String,
    },
}

/// Performs the transfer for a claimed job. Must return promptly after
/// `control.pause()` or `control.cancel()`.
pub trait JobRunner: Send + Sync + 'static {
    fn run(&self, job: Job, control: DownloadControl) -> impl Future<Output = RunResult> + Send;
    /// Deletes the partial install of a cancelled job (the player chose not to keep it).
    fn discard(&self, job: &Job) -> impl Future<Output = ()> + Send;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Intent {
    Run,
    Pause,
    Cancel { keep_partial: bool },
}

struct Active {
    package: PackageRef,
    control: DownloadControl,
    intent: Intent,
}

pub struct InstallQueue<R> {
    db: Db,
    bus: EventBus,
    runner: Arc<R>,
    wake: Notify,
    active: Mutex<Option<Active>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn phase(p: Phase) -> InstallPhase {
    match p {
        Phase::Queued => InstallPhase::Queued,
        Phase::VerifyingManifest => InstallPhase::VerifyingManifest,
        Phase::Allocating => InstallPhase::Allocating,
        Phase::Downloading => InstallPhase::Downloading,
        Phase::Finalizing | Phase::Done => InstallPhase::Finalizing,
        Phase::Paused => InstallPhase::Paused,
    }
}

impl<R: JobRunner> InstallQueue<R> {
    pub fn new(db: Db, bus: EventBus, runner: Arc<R>) -> Arc<Self> {
        Arc::new(Self {
            db,
            bus,
            runner,
            wake: Notify::new(),
            active: Mutex::new(None),
        })
    }

    /// Removes a waiting (queued, paused or failed) job; its partial files stay.
    pub async fn remove(&self, package: PackageRef) -> Result<(), JobStoreError> {
        download_jobs::remove_waiting(&self.db, package).await
    }

    /// New order of the waiting jobs (see `download_jobs::reorder`).
    pub async fn reorder(&self, packages: Vec<PackageRef>) -> Result<(), JobStoreError> {
        download_jobs::reorder(&self.db, packages).await
    }

    pub async fn enqueue(
        &self,
        package: PackageRef,
        version_id: Uuid,
        library_id: Uuid,
        kind: JobKind,
        options: JobOptions,
    ) -> Result<Job, JobStoreError> {
        let job = download_jobs::enqueue(&self.db, package, version_id, library_id, kind, options)
            .await?;
        self.wake.notify_one();
        Ok(job)
    }

    /// Pauses a running job (the worker records it once the transfer stops) or
    /// a waiting one (recorded immediately).
    pub async fn pause(&self, package: PackageRef) -> Result<(), JobStoreError> {
        {
            let mut active = lock(&self.active);
            if let Some(a) = active.as_mut().filter(|a| a.package == package) {
                a.intent = Intent::Pause;
                a.control.pause();
                return Ok(());
            }
        }
        download_jobs::transition(&self.db, package, JobTransition::PauseQueued).await
    }

    pub async fn resume(&self, package: PackageRef) -> Result<(), JobStoreError> {
        download_jobs::transition(&self.db, package, JobTransition::Resume).await?;
        self.wake.notify_one();
        Ok(())
    }

    pub async fn retry(&self, package: PackageRef) -> Result<(), JobStoreError> {
        download_jobs::transition(&self.db, package, JobTransition::Retry).await?;
        self.wake.notify_one();
        Ok(())
    }

    /// Cancels a job. `keep_partial` decides whether the partial install stays
    /// on disk (02 §7.10).
    pub async fn cancel(
        &self,
        package: PackageRef,
        keep_partial: bool,
    ) -> Result<(), JobStoreError> {
        {
            let mut active = lock(&self.active);
            if let Some(a) = active.as_mut().filter(|a| a.package == package) {
                a.intent = Intent::Cancel { keep_partial };
                a.control.cancel();
                return Ok(());
            }
        }
        let job = download_jobs::list(&self.db)
            .await?
            .into_iter()
            .find(|j| j.package == package)
            .ok_or(JobStoreError::NotFound)?;
        download_jobs::remove_waiting(&self.db, package).await?;
        if !keep_partial {
            self.runner.discard(&job).await;
        }
        self.bus.publish(AppEvent::InstallFinished(InstallFinished {
            package,
            outcome: InstallOutcome::Cancelled {
                kept_partial: keep_partial,
            },
        }));
        Ok(())
    }

    /// Runs jobs until `shutdown` is cancelled. A running job is paused on
    /// shutdown and stays `active` in the table; the next start requeues it.
    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) {
        // No worker survives a restart: a job left `active` goes back in the queue.
        if let Err(error) = download_jobs::recover_active(&self.db).await {
            tracing::error!(%error, "requeueing interrupted install jobs failed");
        }
        loop {
            let claimed = match download_jobs::claim_next(&self.db).await {
                Ok(job) => job,
                Err(error) => {
                    tracing::error!(%error, "claiming the next install job failed");
                    None
                }
            };
            match claimed {
                Some(job) => {
                    if !self.process(job, &shutdown).await {
                        return;
                    }
                }
                None => {
                    tokio::select! {
                        () = shutdown.cancelled() => return,
                        () = self.wake.notified() => {}
                    }
                }
            }
        }
    }

    /// Returns `false` when the launcher is shutting down.
    async fn process(&self, job: Job, shutdown: &CancellationToken) -> bool {
        let package = job.package;
        let control = DownloadControl::new(None);
        *lock(&self.active) = Some(Active {
            package,
            control: control.clone(),
            intent: Intent::Run,
        });

        let forward = {
            let bus = self.bus.clone();
            let mut rx = control.progress();
            let stop = CancellationToken::new();
            let stop_child = stop.clone();
            let task = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = stop_child.cancelled() => return,
                        changed = rx.changed() => if changed.is_err() { return },
                    }
                    let p = rx.borrow_and_update().clone();
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
            });
            (stop, task)
        };

        let run = self.runner.run(job.clone(), control.clone());
        tokio::pin!(run);
        let mut stopping = false;
        let result = loop {
            tokio::select! {
                result = &mut run => break result,
                () = shutdown.cancelled(), if !stopping => {
                    stopping = true;
                    control.pause();
                }
            }
        };
        forward.0.cancel();
        let _ = forward.1.await;

        let intent = lock(&self.active).take().map_or(Intent::Run, |a| a.intent);
        if stopping {
            // Leave the row `active`; startup recovery requeues it.
            return false;
        }
        self.finish(&job, result, intent).await;
        true
    }

    async fn finish(&self, job: &Job, result: RunResult, intent: Intent) {
        let package = job.package;
        let outcome = match result {
            RunResult::Installed => {
                self.record(download_jobs::finish_active(&self.db, package).await);
                Some(InstallOutcome::Installed)
            }
            RunResult::Paused(reason) => {
                let reason = match reason {
                    PauseReason::User => "user",
                    PauseReason::DiskFull => "disk_full",
                };
                self.record(
                    download_jobs::transition(
                        &self.db,
                        package,
                        JobTransition::PauseActive {
                            reason: reason.to_owned(),
                        },
                    )
                    .await,
                );
                None
            }
            RunResult::Cancelled => {
                let keep_partial = match intent {
                    Intent::Cancel { keep_partial } => keep_partial,
                    _ => true,
                };
                self.record(download_jobs::finish_active(&self.db, package).await);
                if !keep_partial {
                    self.runner.discard(job).await;
                }
                Some(InstallOutcome::Cancelled {
                    kept_partial: keep_partial,
                })
            }
            RunResult::Failed { code, message } => {
                self.record(
                    download_jobs::transition(
                        &self.db,
                        package,
                        JobTransition::FailActive {
                            error: code.clone(),
                        },
                    )
                    .await,
                );
                Some(InstallOutcome::Failed { code, message })
            }
        };
        if let Some(outcome) = outcome {
            self.bus.publish(AppEvent::InstallFinished(InstallFinished {
                package,
                outcome,
            }));
        }
    }

    fn record(&self, result: Result<(), JobStoreError>) {
        if let Err(error) = result {
            tracing::error!(%error, "recording an install job state failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::libraries;
    use std::collections::VecDeque;
    use std::time::Duration;
    use tokio::sync::{mpsc, oneshot};

    /// Scripted runner: each run either returns the next scripted result
    /// immediately or, for `Block`, waits for pause/cancel on its control.
    enum Step {
        Now(RunResult),
        BlockUntilControl,
    }

    struct Fake {
        steps: Mutex<VecDeque<Step>>,
        started: mpsc::UnboundedSender<PackageRef>,
        discarded: Mutex<Vec<PackageRef>>,
    }

    impl JobRunner for Fake {
        async fn run(&self, job: Job, control: DownloadControl) -> RunResult {
            let _ = self.started.send(job.package);
            let step = lock(&self.steps)
                .pop_front()
                .unwrap_or(Step::Now(RunResult::Installed));
            match step {
                Step::Now(result) => result,
                Step::BlockUntilControl => {
                    if control.interrupted().await {
                        RunResult::Cancelled
                    } else {
                        RunResult::Paused(PauseReason::User)
                    }
                }
            }
        }
        async fn discard(&self, job: &Job) {
            lock(&self.discarded).push(job.package);
        }
    }

    struct Rig {
        queue: Arc<InstallQueue<Fake>>,
        db: Db,
        runner: Arc<Fake>,
        started: mpsc::UnboundedReceiver<PackageRef>,
        server: Uuid,
        library: Uuid,
        shutdown: CancellationToken,
        bus: EventBus,
        _dir: tempfile::TempDir,
    }

    async fn rig(steps: Vec<Step>) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("launcher.sqlite3")).unwrap();
        let library = libraries::add(&db, dir.path(), "Games")
            .await
            .unwrap()
            .root
            .id;
        let server = Uuid::now_v7();
        let id = server.to_string();
        db.call(move |conn| {
            conn.execute(
                "INSERT INTO servers (id, url, name, root_public_key, root_fingerprint, added_at)
                 VALUES (?1, 'https://example.test', 'Test', zeroblob(32), 'VG1', 0)",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let (tx, started) = mpsc::unbounded_channel();
        let runner = Arc::new(Fake {
            steps: Mutex::new(steps.into()),
            started: tx,
            discarded: Mutex::new(Vec::new()),
        });
        let bus = EventBus::new();
        let queue = InstallQueue::new(db.clone(), bus.clone(), Arc::clone(&runner));
        let shutdown = CancellationToken::new();
        tokio::spawn(Arc::clone(&queue).run(shutdown.clone()));
        Rig {
            queue,
            db,
            runner,
            started,
            server,
            library,
            shutdown,
            bus,
            _dir: dir,
        }
    }

    impl Rig {
        fn pkg(&self) -> PackageRef {
            PackageRef {
                server_id: self.server,
                package_id: Uuid::now_v7(),
            }
        }
        async fn add(&self, p: PackageRef) {
            self.queue
                .enqueue(
                    p,
                    Uuid::now_v7(),
                    self.library,
                    JobKind::Install,
                    JobOptions::default(),
                )
                .await
                .unwrap();
        }
        async fn job(&self, p: PackageRef) -> Option<Job> {
            download_jobs::list(&self.db)
                .await
                .unwrap()
                .into_iter()
                .find(|j| j.package == p)
        }
    }

    async fn next_finished(rx: &mut tokio::sync::broadcast::Receiver<AppEvent>) -> InstallFinished {
        loop {
            if let Ok(AppEvent::InstallFinished(f)) =
                tokio::time::timeout(Duration::from_secs(10), rx.recv())
                    .await
                    .unwrap()
            {
                return f;
            }
        }
    }

    #[tokio::test]
    async fn jobs_run_one_at_a_time_in_order_and_are_removed_when_installed() {
        let mut rig = rig(vec![
            Step::Now(RunResult::Installed),
            Step::Now(RunResult::Installed),
        ])
        .await;
        let mut events = rig.bus.subscribe();
        let (a, b) = (rig.pkg(), rig.pkg());
        rig.add(a).await;
        rig.add(b).await;
        assert_eq!(next_finished(&mut events).await.package, a);
        assert_eq!(next_finished(&mut events).await.package, b);
        assert_eq!(rig.started.recv().await, Some(a));
        assert_eq!(rig.started.recv().await, Some(b));
        assert!(download_jobs::list(&rig.db).await.unwrap().is_empty());
        rig.shutdown.cancel();
    }

    #[tokio::test]
    async fn disk_full_pauses_with_a_reason_and_resume_runs_it_again() {
        let mut rig = rig(vec![Step::Now(RunResult::Paused(PauseReason::DiskFull))]).await;
        let p = rig.pkg();
        rig.add(p).await;
        rig.started.recv().await.unwrap();
        // Wait for the pause to be recorded.
        let job = loop {
            if let Some(j) = rig
                .job(p)
                .await
                .filter(|j| j.state == download_jobs::JobState::Paused)
            {
                break j;
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(job.error.as_deref(), Some("disk_full"));
        let mut events = rig.bus.subscribe();
        rig.queue.resume(p).await.unwrap();
        assert_eq!(next_finished(&mut events).await.package, p);
        assert!(rig.job(p).await.is_none());
        rig.shutdown.cancel();
    }

    #[tokio::test]
    async fn failure_is_kept_for_retry_and_reported() {
        let rig = rig(vec![Step::Now(RunResult::Failed {
            code: "integrity".into(),
            message: "chunk mismatch".into(),
        })])
        .await;
        let mut events = rig.bus.subscribe();
        let p = rig.pkg();
        rig.add(p).await;
        let finished = next_finished(&mut events).await;
        assert!(
            matches!(finished.outcome, InstallOutcome::Failed { ref code, .. } if code == "integrity")
        );
        let job = rig.job(p).await.unwrap();
        assert_eq!(job.state, download_jobs::JobState::Failed);
        rig.queue.retry(p).await.unwrap();
        assert_eq!(next_finished(&mut events).await.outcome_kind(), "installed");
        rig.shutdown.cancel();
    }

    #[tokio::test]
    async fn cancelling_the_active_job_honours_keep_or_delete() {
        for keep in [true, false] {
            let mut rig = rig(vec![Step::BlockUntilControl]).await;
            let mut events = rig.bus.subscribe();
            let p = rig.pkg();
            rig.add(p).await;
            rig.started.recv().await.unwrap();
            rig.queue.cancel(p, keep).await.unwrap();
            let finished = next_finished(&mut events).await;
            assert!(
                matches!(finished.outcome, InstallOutcome::Cancelled { kept_partial } if kept_partial == keep)
            );
            assert_eq!(lock(&rig.runner.discarded).len(), usize::from(!keep));
            assert!(rig.job(p).await.is_none());
            rig.shutdown.cancel();
        }
    }

    #[tokio::test]
    async fn pausing_the_active_job_lets_the_next_waiting_job_wait_for_resume() {
        let mut rig = rig(vec![Step::BlockUntilControl]).await;
        let (a, b) = (rig.pkg(), rig.pkg());
        rig.add(a).await;
        rig.add(b).await;
        rig.started.recv().await.unwrap();
        rig.queue.pause(a).await.unwrap();
        // b starts once a is paused.
        assert_eq!(rig.started.recv().await, Some(b));
        assert_eq!(
            rig.job(a).await.unwrap().state,
            download_jobs::JobState::Paused
        );
        rig.queue.cancel(a, true).await.unwrap();
        assert!(rig.job(a).await.is_none());
        rig.shutdown.cancel();
    }

    #[tokio::test]
    async fn shutdown_leaves_the_job_for_startup_recovery() {
        let mut rig = rig(vec![Step::BlockUntilControl]).await;
        let p = rig.pkg();
        rig.add(p).await;
        rig.started.recv().await.unwrap();
        rig.shutdown.cancel();
        let (tx, rx) = oneshot::channel::<()>();
        drop(tx);
        let _ = rx.await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            rig.job(p).await.unwrap().state,
            download_jobs::JobState::Active
        );
        assert_eq!(download_jobs::recover_active(&rig.db).await.unwrap(), 1);
    }

    impl InstallFinished {
        fn outcome_kind(&self) -> &'static str {
            match self.outcome {
                InstallOutcome::Installed => "installed",
                InstallOutcome::Cancelled { .. } => "cancelled",
                InstallOutcome::Failed { .. } => "failed",
            }
        }
    }
}
