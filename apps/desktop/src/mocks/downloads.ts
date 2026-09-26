// The install queue as the Rust core keeps it: jobs waiting, running, paused or failed, and what
// finished. In the browser (dev mode, Playwright) a small simulator moves the running job through its
// phases and emits `install-progress`; unit tests leave it off and emit progress themselves.
import type {
  DownloadError,
  DownloadHistoryEntry,
  DownloadJob,
  DownloadKind,
  InstalledPackage,
  InstallPhase,
  PackageRef,
  PauseReason,
} from "../ipc";
import { events } from "../ipc";
import type { MockPackage } from "./catalog";
import { BASE_TIME } from "./library";
import { fail, type Handler } from "./runtime";

export interface DownloadsState {
  downloads: DownloadJob[];
  downloadHistory: DownloadHistoryEntry[];
  /** Browser only: simulated transfer rate of the running job (bytes/s); 0 = no simulation. */
  downloadRate: number;
}

export function defaultDownloadsState(): DownloadsState {
  return { downloads: [], downloadHistory: [], downloadRate: 0 };
}

const sameRef = (a: PackageRef, b: unknown) =>
  typeof b === "object" &&
  b !== null &&
  (b as PackageRef).server_id === a.server_id &&
  (b as PackageRef).package_id === a.package_id;

const GiB = 1024 ** 3;

export function makeJob(
  pkg: Pick<MockPackage, "id" | "slug" | "title">,
  serverId: string,
  libraryId: string,
  patch: Partial<DownloadJob> = {},
): DownloadJob {
  return {
    package: { server_id: serverId, package_id: pkg.id },
    title: pkg.title,
    cover_url: null,
    kind: "install",
    version_label: "1.0.0",
    library_id: libraryId,
    bytes_done: 0,
    bytes_total: 10 * GiB,
    state: { kind: "queued" },
    queued_at: new Date(BASE_TIME).toISOString(),
    ...patch,
  };
}

/**
 * One of every state, for the `ready` browser preset and screenshots. Catalog entries 50–62 are not
 * installed and not used by the e2e tests.
 */
export function makeDownloads(
  catalog: MockPackage[],
  installs: InstalledPackage[],
  serverId: string,
  libraryId: string,
  libraryPath: string,
): Pick<DownloadsState, "downloads" | "downloadHistory"> {
  const at = (i: number) => {
    const pkg = catalog[i];
    if (!pkg) throw new Error(`catalog fixture ${i}`);
    return pkg;
  };
  const updating = installs[1];
  const job = (i: number, patch: Partial<DownloadJob>) =>
    makeJob(at(i), serverId, libraryId, {
      version_label: at(i).version_label,
      bytes_total: at(i).total_size,
      ...patch,
    });
  const downloads: DownloadJob[] = [
    job(50, { state: { kind: "active" }, bytes_done: Math.floor(at(50).total_size * 0.37) }),
    ...(updating
      ? [
          makeJob(
            { id: updating.package.package_id, slug: updating.slug, title: updating.title },
            serverId,
            libraryId,
            { kind: "update" as DownloadKind, version_label: "2.0.0", bytes_total: 3 * GiB },
          ),
        ]
      : []),
    job(51, {}),
    job(52, { state: { kind: "paused", reason: { kind: "user" } }, bytes_done: 2 * GiB }),
    job(53, {
      state: {
        kind: "paused",
        reason: {
          kind: "disk_full",
          library_path: libraryPath,
          required_bytes: 4 * GiB,
          available_bytes: 1.5 * GiB,
        },
      },
      bytes_done: Math.floor(at(53).total_size * 0.6),
    }),
    job(55, {
      state: { kind: "failed", error: { kind: "damaged_file", reported: true } },
      bytes_done: Math.floor(at(55).total_size * 0.81),
    }),
    job(56, { state: { kind: "failed", error: { kind: "signature_invalid" } } }),
  ];
  const finished = (i: number, hoursAgo: number, patch: Partial<DownloadHistoryEntry>) => ({
    id: `history-${i}`,
    package: { server_id: serverId, package_id: at(i).id },
    title: at(i).title,
    kind: "install" as DownloadKind,
    version_label: at(i).version_label,
    bytes_total: at(i).total_size,
    finished_at: new Date(BASE_TIME - hoursAgo * 3_600_000).toISOString(),
    outcome: { kind: "installed" as const },
    ...patch,
  });
  const downloadHistory: DownloadHistoryEntry[] = [
    finished(60, 2, {}),
    finished(3, 5, { kind: "update", version_label: "1.3.0" }),
    finished(61, 26, { outcome: { kind: "cancelled", kept_partial: true } }),
    finished(62, 50, {
      outcome: { kind: "failed", code: "io", message: "Permission denied (/mnt/games)" },
    }),
    finished(11, 80, { kind: "repair" }),
  ];
  return { downloads, downloadHistory };
}

export function downloadHandlers(
  state: DownloadsState & { installs: InstalledPackage[] },
): Record<string, Handler> {
  const find = (ref: unknown): DownloadJob => {
    const job = state.downloads.find((j) => sameRef(j.package, ref));
    if (!job) fail({ kind: "not_found" });
    return job;
  };
  const set = (ref: PackageRef, patch: Partial<DownloadJob>) => {
    state.downloads = state.downloads.map((j) =>
      sameRef(j.package, ref) ? { ...j, ...patch } : j,
    );
  };
  const changed = () => {
    promote(state);
    void events.downloadsChanged.emit({});
  };

  return {
    downloads_list: () => ({ jobs: state.downloads, history: state.downloadHistory }),
    download_pause: (args) => {
      const job = find(args.package);
      if (job.state.kind === "active" || job.state.kind === "queued")
        set(job.package, { state: { kind: "paused", reason: { kind: "user" } } });
      changed();
      return null;
    },
    download_resume: (args) => {
      const job = find(args.package);
      if (job.state.kind === "paused") set(job.package, { state: { kind: "queued" } });
      changed();
      return null;
    },
    download_retry: (args) => {
      const job = find(args.package);
      if (job.state.kind === "failed") set(job.package, { state: { kind: "queued" } });
      changed();
      return null;
    },
    download_cancel: (args) => {
      const job = find(args.package);
      const keep = Boolean(args.keepPartial);
      state.downloads = state.downloads.filter((j) => j !== job);
      finish(state, job, { kind: "cancelled", kept_partial: keep });
      if (job.kind === "install") {
        state.installs = keep
          ? state.installs.map((i) =>
              sameRef(i.package, job.package) ? { ...i, state: "incomplete" } : i,
            )
          : state.installs.filter((i) => !sameRef(i.package, job.package));
      } else {
        state.installs = state.installs.map((i) =>
          sameRef(i.package, job.package) ? { ...i, state: "installed" } : i,
        );
      }
      void events.installFinished.emit({
        package: job.package,
        outcome: { kind: "cancelled", kept_partial: keep },
      });
      void events.installsChanged.emit({});
      changed();
      return null;
    },
    download_remove: (args) => {
      const job = find(args.package);
      state.downloads = state.downloads.filter((j) => j !== job);
      changed();
      return null;
    },
    downloads_reorder: (args) => {
      const order = (args.packages as PackageRef[]) ?? [];
      const running = state.downloads.filter((j) => j.state.kind === "active");
      const waiting = state.downloads.filter((j) => j.state.kind !== "active");
      const rank = (j: DownloadJob) => {
        const i = order.findIndex((r) => sameRef(j.package, r));
        return i === -1 ? order.length : i;
      };
      state.downloads = [...running, ...[...waiting].sort((a, b) => rank(a) - rank(b))];
      changed();
      return null;
    },
    downloads_history_clear: () => {
      state.downloadHistory = [];
      void events.downloadsChanged.emit({});
      return null;
    },
  };
}

/** One running job at a time (the default setting): the first queued job starts when none runs. */
function promote(state: DownloadsState): void {
  if (state.downloads.some((j) => j.state.kind === "active")) return;
  const next = state.downloads.find((j) => j.state.kind === "queued");
  if (!next) return;
  state.downloads = state.downloads.map((j) =>
    j === next ? { ...j, state: { kind: "active" } } : j,
  );
}

function finish(
  state: DownloadsState,
  job: DownloadJob,
  outcome: DownloadHistoryEntry["outcome"],
): void {
  state.downloadHistory = [
    {
      id: `history-${Date.now()}-${job.package.package_id}`,
      package: job.package,
      title: job.title,
      kind: job.kind,
      version_label: job.version_label,
      bytes_total: job.bytes_total,
      finished_at: new Date().toISOString(),
      outcome,
    },
    ...state.downloadHistory,
  ].slice(0, 100);
}

/** Adds a job for a new install (used by `install_start`). */
export function queueInstall(state: DownloadsState, job: DownloadJob): void {
  state.downloads = [...state.downloads, job];
  promote(state);
  void events.downloadsChanged.emit({});
}

/** Moves a paused or failed job's reason for tests and the browser (e.g. the disk filled up). */
export function setJobState(
  state: DownloadsState,
  ref: PackageRef,
  jobState:
    | { kind: "paused"; reason: PauseReason }
    | { kind: "failed"; error: DownloadError }
    | { kind: "queued" },
): void {
  state.downloads = state.downloads.map((j) =>
    sameRef(j.package, ref) ? { ...j, state: jobState } : j,
  );
  promote(state);
  void events.downloadsChanged.emit({});
}

const PHASE_TICKS: Partial<Record<InstallPhase, number>> = {
  verifying_manifest: 3,
  allocating: 3,
  finalizing: 4,
};

/**
 * Browser only: every 250 ms, moves the running job forward (signature check, allocation,
 * download at `downloadRate`, finalizing) and reports it like the Rust core does.
 */
export function startDownloadSimulation(
  state: DownloadsState & { installs: InstalledPackage[] },
): () => void {
  const phases = new Map<string, { phase: InstallPhase; ticks: number }>();
  const timer = setInterval(() => {
    if (state.downloadRate <= 0) return;
    const job = state.downloads.find((j) => j.state.kind === "active");
    if (!job) return;
    const key = `${job.package.server_id}/${job.package.package_id}`;
    const current = phases.get(key) ?? {
      phase: job.bytes_done > 0 ? ("downloading" as const) : ("verifying_manifest" as const),
      ticks: 0,
    };
    let { phase, ticks } = current;
    let done = job.bytes_done;
    ticks += 1;
    if (phase === "downloading") {
      done = Math.min(
        job.bytes_total,
        done + (state.downloadRate / 4) * (0.8 + Math.random() * 0.4),
      );
      if (done >= job.bytes_total) {
        phase = "finalizing";
        ticks = 0;
      }
    } else if (ticks >= (PHASE_TICKS[phase] ?? 1)) {
      if (phase === "verifying_manifest") phase = "allocating";
      else if (phase === "allocating") phase = "downloading";
      else if (phase === "finalizing") {
        phases.delete(key);
        state.downloads = state.downloads.filter((j) => j !== job);
        finish(state, job, { kind: "installed" });
        state.installs = state.installs.map((i) =>
          sameRef(i.package, job.package)
            ? { ...i, state: "installed", version_label: job.version_label, update: null }
            : i,
        );
        promote(state);
        void events.installFinished.emit({ package: job.package, outcome: { kind: "installed" } });
        void events.downloadsChanged.emit({});
        return;
      }
      ticks = 0;
    }
    phases.set(key, { phase, ticks });
    state.downloads = state.downloads.map((j) => (j === job ? { ...j, bytes_done: done } : j));
    const remaining = job.bytes_total - done;
    void events.installProgress.emit({
      package: job.package,
      phase,
      bytes_done: done,
      bytes_total: job.bytes_total,
      bytes_per_second: phase === "downloading" ? state.downloadRate : 0,
      eta_seconds: phase === "downloading" ? Math.round(remaining / state.downloadRate) : null,
      connections: phase === "downloading" ? 12 : 0,
    });
  }, 250);
  return () => clearInterval(timer);
}
