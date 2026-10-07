// Uploads in progress and recently finished: live from `publish-progress`, with stop, resume,
// release and remove.
import { Button } from "../../components/Button";
import { Badge } from "../../components/Feedback";
import { ProgressBar } from "../../components/ProgressBar";
import { formatBytes, t } from "../../i18n";
import type { PublishJob } from "../../ipc";
import { canResume, isRunning, jobErrorText, jobProgress, jobStatus, platformLabel } from "./model";
import styles from "./Publish.module.css";

export interface JobActions {
  busy: ReadonlySet<string>;
  cancel: (job: PublishJob) => void;
  resume: (job: PublishJob) => void;
  release: (job: PublishJob) => void;
  dismiss: (job: PublishJob) => void;
}

function JobItem({ job, actions }: { job: PublishJob; actions: JobActions }) {
  const names = { title: job.package_title, version: job.version_label };
  const busy = actions.busy.has(job.id);
  const running = isRunning(job);
  const progress = jobProgress(job);
  const tone =
    job.phase === "failed"
      ? "danger"
      : job.phase === "ready" || job.phase === "published"
        ? "success"
        : "accent";
  return (
    <li className={styles.job} data-job={job.id}>
      <div className={styles.jobHead}>
        <h3 className={styles.jobTitle}>
          {job.package_title} {job.version_label}
        </h3>
        <Badge tone="neutral">{platformLabel(job.platform)}</Badge>
      </div>
      <p className={styles.muted}>{jobStatus(job)}</p>
      <ProgressBar
        label={t("publish.title")}
        hideLabel
        value={job.phase === "preparing" ? null : progress}
        tone={tone}
        valueText={jobStatus(job)}
      />
      {job.error ? (
        <p className={styles.error} role="alert">
          {jobErrorText(job.error)}
        </p>
      ) : null}
      {job.packs.length > 1 ? (
        <details className={styles.packs}>
          <summary>{t("publish.packs", { count: job.packs.length })}</summary>
          <ul>
            {job.packs.map((pack) => (
              <li key={pack.index}>
                {t("publish.pack", { index: pack.index + 1 })} ·{" "}
                {t(`publish.packState.${pack.state}`)} ·{" "}
                {t("publish.progress", {
                  done: formatBytes(pack.bytes_confirmed),
                  total: formatBytes(pack.bytes_total),
                })}
              </li>
            ))}
          </ul>
        </details>
      ) : null}
      <div className={styles.actions}>
        {running ? (
          <Button icon="stop" loading={busy} onClick={() => actions.cancel(job)}>
            {t("publish.cancel", names)}
          </Button>
        ) : null}
        {canResume(job) ? (
          <Button variant="primary" icon="play" loading={busy} onClick={() => actions.resume(job)}>
            {t("publish.resume", names)}
          </Button>
        ) : null}
        {job.phase === "ready" ? (
          <Button variant="primary" icon="cloud" onClick={() => actions.release(job)}>
            {t("publish.release", names)}
          </Button>
        ) : null}
        {!running ? (
          <Button variant="ghost" icon="close" loading={busy} onClick={() => actions.dismiss(job)}>
            {t("publish.dismiss", names)}
          </Button>
        ) : null}
      </div>
    </li>
  );
}

export function JobList({ jobs, actions }: { jobs: readonly PublishJob[]; actions: JobActions }) {
  return (
    <ul className={styles.jobs}>
      {jobs.map((job) => (
        <JobItem key={job.id} job={job} actions={actions} />
      ))}
    </ul>
  );
}
