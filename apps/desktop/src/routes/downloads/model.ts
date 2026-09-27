// Pure helpers for the Downloads screen: what each job shows, which actions it offers, how failures
// read, and queue reordering. The screen itself is in DownloadsPage.tsx.
import type { PackageRef } from "../../bindings";
import { formatBytes, formatDuration, formatRate, t } from "../../i18n";
import type {
  DownloadActionError,
  DownloadError,
  DownloadJob,
  InstallProgress,
  PauseReason,
} from "../../ipc";

export const refKey = (ref: PackageRef) => `${ref.server_id}/${ref.package_id}`;

/** Live progress of running jobs, by `refKey`. */
export type ProgressMap = ReadonlyMap<string, InstallProgress>;

/** What a job's progress bar and status line show right now. */
export interface JobView {
  /** 0–1, or null for an indeterminate bar (signature check, allocation, finishing). */
  fraction: number | null;
  /** Visible status line, e.g. "Downloading · 1.2 GB of 26.5 GB · 45 MB/s · 9 minutes left". */
  status: string;
  /** Short text read by screen readers as the bar's value. */
  valueText: string;
}

const INDETERMINATE = new Set(["verifying_manifest", "allocating", "finalizing"]);

export function jobView(job: DownloadJob, live: InstallProgress | undefined): JobView {
  const total = live?.bytes_total ?? job.bytes_total;
  const done = live?.bytes_done ?? job.bytes_done;
  const fraction = total > 0 ? Math.min(1, done / total) : 0;
  const amount = t("downloads.of", { done: formatBytes(done), total: formatBytes(total) });
  if (job.state.kind === "active" && live) {
    const phase = t(`downloads.phase.${live.phase}`);
    if (INDETERMINATE.has(live.phase)) return { fraction: null, status: phase, valueText: phase };
    const parts = [phase, amount];
    if (live.phase === "downloading") {
      if (live.bytes_per_second > 0) parts.push(formatRate(live.bytes_per_second));
      if (live.eta_seconds !== null)
        parts.push(t("downloads.left", { time: formatDuration(live.eta_seconds) }));
      if (live.connections > 0) parts.push(t("downloads.connections", { count: live.connections }));
    }
    return { fraction, status: parts.join(" · "), valueText: amount };
  }
  if (job.state.kind === "active")
    return {
      fraction,
      status: `${t("downloads.phase.downloading")} · ${amount}`,
      valueText: amount,
    };
  if (job.state.kind === "queued")
    return { fraction, status: `${t("downloads.waiting")} · ${amount}`, valueText: amount };
  if (job.state.kind === "paused")
    return { fraction, status: `${t("downloads.phase.paused")} · ${amount}`, valueText: amount };
  return { fraction, status: amount, valueText: amount };
}

export function pauseText(reason: PauseReason): string {
  switch (reason.kind) {
    case "user":
      return t("downloads.paused.user");
    case "disk_full":
      return t("downloads.paused.disk_full", {
        path: reason.library_path,
        required: formatBytes(reason.required_bytes),
        available: formatBytes(reason.available_bytes),
      });
    case "library_offline":
      return t("downloads.paused.library_offline", { path: reason.library_path });
    case "offline":
      return t("downloads.paused.offline");
  }
}

/** How a failure is shown and what the player can do about it. */
export interface FailureView {
  tone: "danger" | "warning";
  title: string;
  text: string;
  /**
   * Whether "Try again" is offered. Never for signature and trust failures: retrying can't help,
   * and the player should not be nudged past a security check.
   */
  retry: boolean;
  security: boolean;
}

export function failureView(error: DownloadError): FailureView {
  switch (error.kind) {
    case "damaged_file":
      return {
        tone: "danger",
        title: t("downloads.failed.damagedTitle"),
        text: error.reported
          ? t("downloads.failed.damagedReported")
          : t("downloads.failed.damagedNotReported"),
        retry: true,
        security: false,
      };
    case "signature_invalid":
    case "untrusted_key":
    case "trust_expired":
      return {
        tone: "danger",
        title: t("downloads.failed.securityTitle"),
        text: t(`downloads.failed.${error.kind}`),
        retry: false,
        security: true,
      };
    case "version_unavailable":
      return {
        tone: "warning",
        title: t("downloads.failed.versionUnavailableTitle"),
        text: t("downloads.failed.versionUnavailable"),
        retry: false,
        security: false,
      };
    case "io":
      return {
        tone: "danger",
        title: t("downloads.failed.ioTitle"),
        text: t("downloads.failed.io", { path: error.path, detail: error.detail }),
        retry: true,
        security: false,
      };
    case "server":
      return {
        tone: "danger",
        title: t("downloads.failed.serverTitle"),
        text: t("downloads.failed.server", { code: error.code }),
        retry: true,
        security: false,
      };
  }
}

export function actionErrorText(error: DownloadActionError): string {
  switch (error.kind) {
    case "not_found":
      return t("downloads.errors.not_found");
    case "insufficient_space":
      return t("downloads.errors.insufficient_space", {
        required: formatBytes(error.required_bytes),
        available: formatBytes(error.available_bytes),
      });
    case "library_offline":
      return t("downloads.errors.library_offline", { path: error.library_path });
    case "offline":
      return t("downloads.errors.offline");
    case "io":
      return t("downloads.errors.io", { detail: error.detail });
  }
}

/** Waiting jobs (everything that isn't running), in queue order. */
export const waitingJobs = (jobs: readonly DownloadJob[]) =>
  jobs.filter((j) => j.state.kind !== "active");

/**
 * The new order of the waiting jobs after moving `ref` to `index` (clamped), or null when nothing
 * changes.
 */
export function moveTo(
  jobs: readonly DownloadJob[],
  ref: PackageRef,
  index: number,
): PackageRef[] | null {
  const waiting = waitingJobs(jobs).map((j) => j.package);
  const from = waiting.findIndex((r) => refKey(r) === refKey(ref));
  if (from === -1) return null;
  const to = Math.max(0, Math.min(waiting.length - 1, index));
  if (to === from) return null;
  const [moved] = waiting.splice(from, 1);
  if (!moved) return null;
  waiting.splice(to, 0, moved);
  return waiting;
}
