// One job in the queue: cover, title and version, progress with speed, time left and connections,
// why it waits or failed, and what the player can do (pause, resume, try again, cancel, reorder).
import type { KeyboardEvent } from "react";
import { useNavigate } from "react-router";
import { Button, IconButton } from "../../components/Button";
import { Menu, type MenuEntry } from "../../components/Menu";
import { Notice } from "../../components/Notice";
import { ProgressBar } from "../../components/ProgressBar";
import { t } from "../../i18n";
import type { DownloadJob, InstallProgress } from "../../ipc";
import { initials, placeholderHue } from "../library/model";
import styles from "./Downloads.module.css";
import { failureView, jobView, pauseText, refKey } from "./model";

export interface ItemActions {
  pause: (job: DownloadJob) => void;
  resume: (job: DownloadJob) => void;
  retry: (job: DownloadJob) => void;
  remove: (job: DownloadJob) => void;
  cancel: (job: DownloadJob) => void;
  /** Moves a waiting job to `index` among the waiting jobs. */
  move: (job: DownloadJob, index: number) => void;
  busy: ReadonlySet<string>;
}

export function DownloadItem({
  job,
  live,
  position,
  waitingCount,
  actions,
}: {
  job: DownloadJob;
  live: InstallProgress | undefined;
  /** Index among the waiting jobs, or null for a running job. */
  position: number | null;
  waitingCount: number;
  actions: ItemActions;
}) {
  const navigate = useNavigate();
  const key = refKey(job.package);
  const view = jobView(job, live);
  const busy = actions.busy.has(key);
  const failure = job.state.kind === "failed" ? failureView(job.state.error) : null;
  const hue = placeholderHue(job.package.package_id);
  const titleId = `download-${key}`;

  const entries: MenuEntry[] = [];
  if (position !== null) {
    entries.push(
      {
        id: "top",
        label: t("downloads.moveTop"),
        icon: "download",
        disabled: position === 0,
        onSelect: () => actions.move(job, 0),
      },
      {
        id: "up",
        label: t("downloads.moveUp"),
        icon: "arrowUp",
        hint: "Alt+↑",
        disabled: position === 0,
        onSelect: () => actions.move(job, position - 1),
      },
      {
        id: "down",
        label: t("downloads.moveDown"),
        hint: "Alt+↓",
        icon: "arrowDown",
        disabled: position >= waitingCount - 1,
        onSelect: () => actions.move(job, position + 1),
      },
      { id: "sep", separator: true },
    );
  }
  entries.push({
    id: "details",
    label: t("downloads.details"),
    icon: "info",
    onSelect: () => navigate(`/package/${job.package.package_id}`),
  });

  // Alt+Up / Alt+Down reorder the waiting jobs from anywhere in the row.
  const onKeyDown = (e: KeyboardEvent<HTMLLIElement>) => {
    if (position === null || !e.altKey || (e.key !== "ArrowUp" && e.key !== "ArrowDown")) return;
    e.preventDefault();
    e.stopPropagation();
    actions.move(job, position + (e.key === "ArrowUp" ? -1 : 1));
  };

  let primary = null;
  if (job.state.kind === "active" || job.state.kind === "queued") {
    primary = (
      <Button
        size="sm"
        icon="pause"
        loading={busy}
        aria-label={t("downloads.pauseTitle", { title: job.title })}
        onClick={() => actions.pause(job)}
        data-action="pause"
      >
        {t("downloads.pause")}
      </Button>
    );
  } else if (job.state.kind === "paused") {
    primary = (
      <Button
        size="sm"
        variant="primary"
        icon="play"
        loading={busy}
        aria-label={t("downloads.resumeTitle", { title: job.title })}
        onClick={() => actions.resume(job)}
        data-action="resume"
      >
        {t("downloads.resume")}
      </Button>
    );
  } else if (failure?.retry) {
    primary = (
      <Button
        size="sm"
        icon="refresh"
        loading={busy}
        aria-label={t("downloads.retryTitle", { title: job.title })}
        onClick={() => actions.retry(job)}
        data-action="retry"
      >
        {t("downloads.retry")}
      </Button>
    );
  }

  return (
    // Alt+arrows on any control of the row reorder its job (see onKeyDown).
    <li
      className={styles.item}
      data-job={key}
      data-state={job.state.kind}
      aria-labelledby={titleId}
      onKeyDown={onKeyDown}
    >
      <span
        className={styles.thumb}
        style={{
          background: `linear-gradient(160deg, hsl(${hue} 45% 32%), hsl(${(hue + 40) % 360} 55% 16%))`,
        }}
        aria-hidden="true"
      >
        {initials(job.title)}
      </span>
      <div className={styles.body}>
        <div className={styles.titleRow}>
          <h3 id={titleId} className={styles.title}>
            {job.title}
          </h3>
          <span className={styles.kind}>
            {t(`downloads.kind.${job.kind}`, { version: job.version_label })}
          </span>
        </div>
        {failure ? null : (
          <>
            <ProgressBar
              label={t("downloads.progressLabel", { title: job.title })}
              hideLabel
              size="sm"
              value={view.fraction}
              valueText={view.valueText}
            />
            <p className={styles.status}>{view.status}</p>
          </>
        )}
        {job.state.kind === "paused" && job.state.reason.kind !== "user" ? (
          <Notice tone="warning" role="status">
            <span className={styles.noticeBody}>
              <span>{pauseText(job.state.reason)}</span>
              {job.state.reason.kind === "disk_full" ? (
                <span>
                  <Button size="sm" onClick={() => navigate("/settings/storage")}>
                    {t("downloads.openStorage")}
                  </Button>
                </span>
              ) : null}
            </span>
          </Notice>
        ) : null}
        {failure ? (
          <Notice tone={failure.tone} role="alert">
            <span className={styles.noticeBody}>
              <strong>{failure.title}</strong>
              <span>{failure.text}</span>
            </span>
          </Notice>
        ) : null}
      </div>
      <div className={styles.actions}>
        {primary}
        {job.state.kind === "failed" ? (
          <Button
            size="sm"
            variant={failure?.retry ? "ghost" : "secondary"}
            icon="close"
            loading={busy && !failure?.retry}
            aria-label={t("downloads.removeTitle", { title: job.title })}
            onClick={() => actions.remove(job)}
            data-action="remove"
          >
            {t("downloads.remove")}
          </Button>
        ) : (
          <IconButton
            icon="close"
            size="sm"
            label={t("downloads.cancelTitle", { title: job.title })}
            onClick={() => actions.cancel(job)}
            data-action="cancel"
          />
        )}
        <Menu
          label={t("downloads.menu", { title: job.title })}
          entries={entries}
          trigger={
            <IconButton
              icon="more"
              size="sm"
              label={t("downloads.more", { title: job.title })}
              data-action="more"
            />
          }
        />
      </div>
    </li>
  );
}
