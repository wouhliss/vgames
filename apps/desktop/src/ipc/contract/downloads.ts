// The install queue: installs, updates and repairs waiting, running, paused or failed, and what
// finished (Agent 2, A2-T06/T08; algorithm and error handling in 02-package-format §7). Requested
// shapes; see core.ts for the conventions. Live progress of running jobs comes from the generated
// `install-progress` event; `install-finished` ends a job.
import type { InstallOutcome, PackageRef } from "../../bindings";
import type { AppError } from "./core";
import { call, makeEvents, type Result } from "./runtime";

export type DownloadKind = "install" | "update" | "repair";

/** Why a job waits (02-package-format §7.10). Every reason except `user` clears by itself. */
export type PauseReason =
  /** The player paused it. */
  | { kind: "user" }
  /** The disk filled up mid-install; the job continues once enough space is free. */
  | { kind: "disk_full"; library_path: string; required_bytes: number; available_bytes: number }
  /** The library's drive was disconnected; the job continues when it is back. */
  | { kind: "library_offline"; library_path: string }
  /** The server can't be reached; the job retries with backoff. */
  | { kind: "offline" };

/** Why a job stopped for good (until the player retries or removes it). */
export type DownloadError =
  /**
   * A chunk failed its hash twice (02 §7.10). `reported`: the launcher sent an integrity report, so
   * the server's admins know.
   */
  | { kind: "damaged_file"; reported: boolean }
  /** The manifest's signature doesn't verify (01-security §3.4). Nothing was written. */
  | { kind: "signature_invalid" }
  /** The manifest was signed with a revoked or unknown publisher key. Nothing was written. */
  | { kind: "untrusted_key" }
  /** The server's trust bundle expired: new downloads wait until it is renewed. */
  | { kind: "trust_expired" }
  /** The version was withdrawn or deleted on the server. */
  | { kind: "version_unavailable" }
  /** Any other I/O error, with the OS message and the path (the journal is kept). */
  | { kind: "io"; path: string; detail: string }
  | { kind: "server"; code: string; message: string };

export type DownloadState =
  | { kind: "active" }
  | { kind: "queued" }
  | { kind: "paused"; reason: PauseReason }
  | { kind: "failed"; error: DownloadError };

export type DownloadJob = {
  package: PackageRef;
  title: string;
  /** `vgimg:` URL of the cached cover. */
  cover_url: string | null;
  kind: DownloadKind;
  /** The version being installed (for a repair: the installed one). */
  version_label: string;
  library_id: string;
  /** Last known progress; running jobs report live progress through `install-progress`. */
  bytes_done: number;
  bytes_total: number;
  state: DownloadState;
  queued_at: string;
};

export type DownloadHistoryEntry = {
  id: string;
  package: PackageRef;
  title: string;
  kind: DownloadKind;
  version_label: string;
  bytes_total: number;
  finished_at: string;
  outcome: InstallOutcome;
};

export type DownloadQueue = {
  /** In queue order: running jobs first, then the rest in the order they will run. */
  jobs: DownloadJob[];
  /** Newest first, at most 100 entries. */
  history: DownloadHistoryEntry[];
};

export type DownloadActionError =
  | { kind: "not_found" }
  | { kind: "insufficient_space"; required_bytes: number; available_bytes: number }
  | { kind: "library_offline"; library_path: string }
  | { kind: "offline" }
  | { kind: "io"; detail: string };

/** The queue or the history changed (jobs added, reordered, paused, finished, removed). */
export type DownloadsChanged = Record<string, never>;

export const downloadCommands = {
  async downloadsList(): Promise<Result<DownloadQueue, AppError>> {
    return call("downloads_list");
  },
  async downloadPause(pkg: PackageRef): Promise<Result<null, DownloadActionError>> {
    return call("download_pause", { package: pkg });
  },
  /** Resumes a paused job (it runs when its turn comes). */
  async downloadResume(pkg: PackageRef): Promise<Result<null, DownloadActionError>> {
    return call("download_resume", { package: pkg });
  },
  /** Queues a failed job again, from its journal. */
  async downloadRetry(pkg: PackageRef): Promise<Result<null, DownloadActionError>> {
    return call("download_retry", { package: pkg });
  },
  /**
   * Stops the job. `keepPartial`: keep the downloaded files so the install can be resumed later
   * (it shows as incomplete in the library); otherwise delete them. An update or repair leaves the
   * installed version as it was.
   */
  async downloadCancel(
    pkg: PackageRef,
    keepPartial: boolean,
  ): Promise<Result<null, DownloadActionError>> {
    return call("download_cancel", { package: pkg, keepPartial });
  },
  /** Removes a failed job from the queue (its partial files are kept, as for a cancel with keep). */
  async downloadRemove(pkg: PackageRef): Promise<Result<null, DownloadActionError>> {
    return call("download_remove", { package: pkg });
  },
  /** The new order of the waiting jobs; running jobs keep running. */
  async downloadsReorder(packages: PackageRef[]): Promise<Result<null, DownloadActionError>> {
    return call("downloads_reorder", { packages });
  },
  async downloadsHistoryClear(): Promise<Result<null, AppError>> {
    return call("downloads_history_clear");
  },
};

export const downloadEvents = makeEvents<{ downloadsChanged: DownloadsChanged }>({
  downloadsChanged: "downloads-changed",
});
