// Launcher admin publishing as the Rust core does it (INS-06): admin packages and versions, folder
// plans, and publishing jobs that upload pack by pack, sign, verify and stop at `ready`.
//
// Fixture triggers, so screens and Playwright reach every outcome without a server:
// - passphrase "wrong" → `wrong_passphrase`;
// - a key file whose name contains "untrusted" → `untrusted_key` (`unknown`), "revoked" → `revoked`;
// - a folder whose name contains "fails-verification" → the job ends `failed` / `verification_failed`;
// - `publishHold` stops every new job halfway through uploading, for cancel-then-resume.
// Unit tests (`publishTickMs` 0) run a job to `ready` or `failed` inside `publish_start`; the
// browser moves it one step per tick and emits `publish-progress` like the core.
import type {
  PackageCreate,
  PackageCreateError,
  PackProgress,
  Platform,
  PublishControlError,
  PublishJob,
  PublishJobError,
  PublishPackage,
  PublishPlan,
  PublishPlanError,
  PublishResume,
  PublishStart,
  PublishStartError,
  PublishVersion,
  Role,
  VersionActionError,
} from "../ipc";
import { events } from "../ipc";
import { fail, type Handler, mockId } from "./runtime";

export interface PublishingState {
  publishPackages: PublishPackage[];
  /** Versions by package id, newest first. */
  publishVersions: Record<string, PublishVersion[]>;
  /** Folders `publish_plan` knows, by path. Any other path is `not_found`. */
  publishFolders: Record<string, PublishPlan>;
  /** What the folder picker returns next (null = cancelled). */
  publishFolderPick: string | null;
  /** What the key picker returns next (null = cancelled). */
  publishKeyPick: string | null;
  publishJobs: PublishJob[];
  /** Stop new and resumed jobs halfway through uploading (until cancelled or resumed with this off). */
  publishHold: boolean;
  /** Browser step interval (ms); 0 = run synchronously (unit tests). */
  publishTickMs: number;
}

const MIB = 1024 ** 2;
const PACK = 512 * MIB;
const BASE = Date.parse("2026-09-01T12:00:00Z");

export const PUBLISH_FOLDERS = {
  ok: "/home/demo/builds/starfall-1.4.0",
  invalid: "/home/demo/builds/broken-names",
  failsVerification: "/home/demo/builds/fails-verification",
} as const;

export const PUBLISH_KEYS = {
  ok: "/home/demo/keys/publisher.vgkey",
  untrusted: "/home/demo/keys/untrusted.vgkey",
  revoked: "/home/demo/keys/revoked.vgkey",
} as const;

function plan(folder: string, total: number, patch: Partial<PublishPlan> = {}): PublishPlan {
  return {
    folder,
    file_count: 1284,
    total_bytes: total,
    pack_count: Math.max(1, Math.ceil(total / PACK)),
    executables: ["Starfall.exe", "tools/CrashReporter.exe"],
    invalid_paths: [],
    invalid_count: 0,
    ...patch,
  };
}

function version(
  packageId: string,
  n: number,
  platform: Platform,
  patch: Partial<PublishVersion> = {},
): PublishVersion {
  return {
    id: mockId("0199e000", n),
    package_id: packageId,
    platform,
    sequence: n,
    version_label: `1.${n}.0`,
    state: "published",
    is_current_release: false,
    failure_reason: null,
    total_size: 3.2 * 1024 * MIB,
    verify_progress: null,
    created_at: new Date(BASE + n * 86_400_000).toISOString(),
    published_at: new Date(BASE + n * 86_400_000 + 3_600_000).toISOString(),
    yanked_at: null,
    ...patch,
  };
}

export function defaultPublishingState(): PublishingState {
  const starfall = mockId("0199d000", 1);
  const ember = mockId("0199d000", 2);
  return {
    publishPackages: [
      {
        id: starfall,
        slug: "starfall",
        title: "Starfall",
        status: "published",
        released_platforms: ["windows-x86_64"],
      },
      {
        id: ember,
        slug: "ember-road",
        title: "Ember Road",
        status: "draft",
        released_platforms: [],
      },
    ],
    publishVersions: {
      [starfall]: [
        version(starfall, 3, "windows-x86_64", { is_current_release: true }),
        version(starfall, 2, "windows-x86_64", {
          state: "yanked",
          yanked_at: new Date(BASE + 3 * 86_400_000).toISOString(),
        }),
        version(starfall, 1, "windows-x86_64", {
          state: "failed",
          failure_reason: "Pack 2 does not match its hash.",
          published_at: null,
        }),
      ],
      [ember]: [],
    },
    publishFolders: {
      [PUBLISH_FOLDERS.ok]: plan(PUBLISH_FOLDERS.ok, 3.4 * 1024 * MIB),
      [PUBLISH_FOLDERS.invalid]: plan(PUBLISH_FOLDERS.invalid, 900 * MIB, {
        invalid_paths: [
          { path: "data/CON.pak", reason: "reserved_name", other: null },
          { path: "data/Save.dat", reason: "case_collision", other: "data/save.dat" },
          { path: "shortcut.lnk-target", reason: "symlink", other: null },
        ],
        invalid_count: 3,
      }),
      [PUBLISH_FOLDERS.failsVerification]: plan(
        PUBLISH_FOLDERS.failsVerification,
        1.1 * 1024 * MIB,
      ),
    },
    publishFolderPick: PUBLISH_FOLDERS.ok,
    publishKeyPick: PUBLISH_KEYS.ok,
    publishJobs: [],
    publishHold: false,
    publishTickMs: 0,
  };
}

/** The steps a job goes through after uploading, in order. */
const AFTER_UPLOAD = ["signing", "uploading_manifest", "finalizing", "verifying"] as const;

function keyError(keyPath: string, passphrase: string): PublishJobError | null {
  const name = keyPath.split(/[\\/]/).pop() ?? "";
  if (!name.endsWith(".vgkey")) return { kind: "invalid_key_file" };
  if (passphrase === "wrong") return { kind: "wrong_passphrase" };
  if (name.includes("untrusted")) return { kind: "untrusted_key", reason: "unknown" };
  if (name.includes("revoked")) return { kind: "untrusted_key", reason: "revoked" };
  return null;
}

function packs(total: number): PackProgress[] {
  const count = Math.max(1, Math.ceil(total / PACK));
  return Array.from({ length: count }, (_, index) => ({
    index,
    bytes_confirmed: 0,
    bytes_total: index === count - 1 ? total - PACK * (count - 1) : PACK,
    state: "waiting" as const,
  }));
}

/** Sets confirmed bytes, filling packs in order (each pack uploads after the one before it). */
function withBytes(job: PublishJob, confirmed: number): PublishJob {
  let left = confirmed;
  const next = job.packs.map((p) => {
    const done = Math.min(p.bytes_total, Math.max(0, left));
    left -= done;
    return {
      ...p,
      bytes_confirmed: done,
      state:
        done >= p.bytes_total
          ? ("done" as const)
          : done > 0
            ? ("uploading" as const)
            : ("waiting" as const),
    };
  });
  return { ...job, bytes_confirmed: confirmed, packs: next };
}

export function publishingHandlers(
  state: PublishingState & { servers: { id: string; account: { role: Role } | null }[] },
): Record<string, Handler> {
  const timers = new Map<string, ReturnType<typeof setInterval>>();
  const failsVerification = new Set<string>();
  let createdVersions = 0;

  function requireAdmin(serverId: unknown) {
    const server = state.servers.find((s) => s.id === serverId);
    if (!server) fail({ kind: "not_found" });
    if (!server.account) fail({ kind: "unauthenticated" });
    if (server.account.role === "user") fail({ kind: "forbidden" });
  }

  function jobById(id: unknown): PublishJob {
    const job = state.publishJobs.find((j) => j.id === id);
    if (!job) fail({ kind: "not_found" } satisfies PublishControlError);
    return job;
  }

  function versionById(id: unknown): PublishVersion {
    for (const list of Object.values(state.publishVersions)) {
      const v = list.find((x) => x.id === id);
      if (v) return v;
    }
    fail({ kind: "not_found" } satisfies VersionActionError);
  }

  function replaceVersion(next: PublishVersion) {
    const list = state.publishVersions[next.package_id] ?? [];
    state.publishVersions[next.package_id] = list.map((v) => (v.id === next.id ? next : v));
  }

  function save(job: PublishJob): PublishJob {
    state.publishJobs = state.publishJobs.map((j) => (j.id === job.id ? job : j));
    if (job.version_id) {
      const v = versionById(job.version_id);
      const remote: PublishVersion["state"] =
        job.phase === "ready"
          ? "ready"
          : job.phase === "failed" && job.error?.kind === "verification_failed"
            ? "failed"
            : job.phase === "verifying"
              ? "verifying"
              : v.state;
      replaceVersion({
        ...v,
        state: remote,
        verify_progress: job.phase === "verifying" ? job.verification : null,
        failure_reason:
          job.error?.kind === "verification_failed" ? job.error.reason : v.failure_reason,
      });
    }
    void events.publishProgress.emit(job);
    return job;
  }

  /** One step: a quarter of the upload, then each phase after it; verification in halves. */
  function step(job: PublishJob): PublishJob {
    switch (job.phase) {
      case "preparing":
        return { ...job, phase: "uploading" };
      case "uploading": {
        const half = Math.ceil(job.bytes_total / 2);
        let confirmed = Math.min(
          job.bytes_total,
          job.bytes_confirmed + Math.ceil(job.bytes_total / 4),
        );
        if (state.publishHold && job.bytes_confirmed < half) confirmed = Math.min(confirmed, half);
        const next = withBytes({ ...job, bytes_per_second: 64 * MIB }, confirmed);
        return confirmed >= job.bytes_total
          ? { ...next, phase: "signing", bytes_per_second: 0, resume_needs_key: false }
          : next;
      }
      case "signing":
      case "uploading_manifest":
      case "finalizing": {
        const at = AFTER_UPLOAD.indexOf(job.phase);
        const phase = AFTER_UPLOAD[at + 1] ?? "verifying";
        return { ...job, phase, verification: phase === "verifying" ? 0 : null };
      }
      case "verifying": {
        const progress = Math.min(1, (job.verification ?? 0) + 0.5);
        if (progress < 1) return { ...job, verification: progress };
        if (failsVerification.has(job.id))
          return {
            ...job,
            phase: "failed",
            verification: null,
            error: { kind: "verification_failed", reason: "Pack 1 does not match its hash." },
          };
        return { ...job, phase: "ready", verification: null };
      }
      default:
        return job;
    }
  }

  const held = (job: PublishJob) =>
    state.publishHold && job.phase === "uploading" && job.bytes_confirmed >= job.bytes_total / 2;
  const running = (job: PublishJob) =>
    ["preparing", "uploading", "signing", "uploading_manifest", "finalizing", "verifying"].includes(
      job.phase,
    );

  function run(id: string): PublishJob {
    let job = jobById(id);
    if (state.publishTickMs <= 0) {
      while (running(job) && !held(job)) job = save(step(job));
      return job;
    }
    const timer = setInterval(() => {
      const current = state.publishJobs.find((j) => j.id === id);
      if (!current || !running(current) || held(current)) {
        clearInterval(timer);
        timers.delete(id);
        return;
      }
      save(step(current));
    }, state.publishTickMs);
    timers.set(id, timer);
    return job;
  }

  function stop(id: string) {
    const timer = timers.get(id);
    if (timer) clearInterval(timer);
    timers.delete(id);
  }

  return {
    publish_packages: (args) => {
      requireAdmin(args.serverId);
      const q = typeof args.query === "string" ? args.query.trim().toLowerCase() : "";
      const items = state.publishPackages.filter(
        (p) => !q || p.title.toLowerCase().includes(q) || p.slug.includes(q),
      );
      return { items, next_cursor: null };
    },
    publish_package_create: (args) => {
      requireAdmin(args.serverId);
      const create = args.create as PackageCreate;
      const title = create.title.trim();
      if (!title || title.length > 200)
        fail({
          kind: "invalid",
          field: "title",
          message: "Enter a title of 1 to 200 characters.",
        } satisfies PackageCreateError);
      const slug =
        create.slug ??
        title
          .toLowerCase()
          .replace(/[^a-z0-9]+/g, "-")
          .replace(/^-+|-+$/g, "")
          .slice(0, 64);
      if (!/^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/.test(slug))
        fail({
          kind: "invalid",
          field: "slug",
          message: "Use lowercase letters, digits and dashes.",
        } satisfies PackageCreateError);
      if (state.publishPackages.some((p) => p.slug === slug))
        fail({ kind: "slug_taken" } satisfies PackageCreateError);
      const pkg: PublishPackage = {
        id: mockId("0199d000", state.publishPackages.length + 1),
        slug,
        title,
        status: "draft",
        released_platforms: [],
      };
      state.publishPackages = [...state.publishPackages, pkg];
      state.publishVersions[pkg.id] = [];
      return pkg;
    },
    publish_versions: (args) => {
      requireAdmin(args.serverId);
      const list = state.publishVersions[args.packageId as string];
      if (!list) fail({ kind: "not_found" });
      return list;
    },
    publish_pick_folder: () => state.publishFolderPick,
    publish_pick_key: () => state.publishKeyPick,
    publish_plan: (args) => {
      const found = state.publishFolders[args.folder as string];
      if (!found) fail({ kind: "not_found" } satisfies PublishPlanError);
      return found;
    },
    publish_start: (args) => {
      requireAdmin(args.serverId);
      const start = args.start as PublishStart;
      const pkg = state.publishPackages.find((p) => p.id === start.package_id);
      if (!pkg) fail({ kind: "not_found" } satisfies PublishStartError);
      const label = start.version_label.trim();
      if (!label || label.length > 64) fail({ kind: "invalid_label" } satisfies PublishStartError);
      const folder = state.publishFolders[start.folder];
      if (!folder) fail({ kind: "not_found" } satisfies PublishStartError);
      if (folder.invalid_count > 0)
        fail({ kind: "invalid_paths", count: folder.invalid_count } satisfies PublishStartError);
      if (start.launch && !folder.executables.includes(start.launch.executable))
        fail({ kind: "invalid_launch" } satisfies PublishStartError);
      const keyFailure = keyError(start.key_path, start.passphrase);
      if (keyFailure) fail(keyFailure satisfies PublishStartError);

      const list = state.publishVersions[pkg.id] ?? [];
      const sequence = Math.max(0, ...list.map((v) => v.sequence)) + 1;
      const created: PublishVersion = {
        ...version(pkg.id, sequence, start.platform),
        id: mockId("0199e100", ++createdVersions),
        version_label: label,
        state: "uploading",
        total_size: folder.total_bytes,
        published_at: null,
      };
      state.publishVersions[pkg.id] = [created, ...list];
      const job: PublishJob = {
        id: mockId("0199f000", state.publishJobs.length + 1),
        server_id: args.serverId as string,
        package_id: pkg.id,
        package_title: pkg.title,
        platform: start.platform,
        version_label: label,
        version_id: created.id,
        phase: "preparing",
        bytes_confirmed: 0,
        bytes_total: folder.total_bytes,
        bytes_per_second: 0,
        packs: packs(folder.total_bytes),
        verification: null,
        error: null,
        resume_needs_key: true,
      };
      if (start.folder.includes("fails-verification")) failsVerification.add(job.id);
      state.publishJobs = [...state.publishJobs, job];
      save(job);
      return run(job.id);
    },
    publish_jobs: () => state.publishJobs,
    publish_cancel: (args) => {
      const job = jobById(args.jobId);
      if (!running(job)) fail({ kind: "conflict", phase: job.phase } satisfies PublishControlError);
      stop(job.id);
      return save({ ...job, phase: "cancelled", bytes_per_second: 0 });
    },
    publish_resume: (args) => {
      const job = jobById(args.jobId);
      if (job.phase !== "cancelled" && !(job.phase === "failed" && job.error?.kind === "remote"))
        fail({ kind: "conflict", phase: job.phase } satisfies PublishControlError);
      const key = args.key as PublishResume | null;
      if (job.resume_needs_key) {
        if (!key) fail({ kind: "wrong_passphrase" } satisfies PublishControlError);
        const keyFailure = keyError(key.key_path, key.passphrase);
        if (keyFailure) fail(keyFailure satisfies PublishControlError);
      }
      const phase = job.bytes_confirmed >= job.bytes_total ? "signing" : "uploading";
      save({ ...job, phase, error: null });
      return run(job.id);
    },
    publish_dismiss: (args) => {
      const job = jobById(args.jobId);
      if (running(job)) fail({ kind: "conflict", phase: job.phase } satisfies PublishControlError);
      stop(job.id);
      if (job.version_id && job.phase !== "published") {
        const v = versionById(job.version_id);
        if (v.state === "uploading" || v.state === "ready")
          replaceVersion({ ...v, state: "aborted" });
      }
      state.publishJobs = state.publishJobs.filter((j) => j.id !== job.id);
      return null;
    },
    publish_release: (args) => {
      requireAdmin(args.serverId);
      const v = versionById(args.versionId);
      if (v.state !== "ready")
        fail({ kind: "conflict", state: v.state } satisfies VersionActionError);
      const list = state.publishVersions[v.package_id] ?? [];
      state.publishVersions[v.package_id] = list.map((x) =>
        x.platform === v.platform ? { ...x, is_current_release: false } : x,
      );
      const released: PublishVersion = {
        ...v,
        state: "published",
        is_current_release: true,
        published_at: new Date().toISOString(),
      };
      replaceVersion(released);
      state.publishPackages = state.publishPackages.map((p) =>
        p.id === v.package_id && !p.released_platforms.includes(v.platform)
          ? { ...p, released_platforms: [...p.released_platforms, v.platform] }
          : p,
      );
      const job = state.publishJobs.find((j) => j.version_id === v.id);
      if (job) save({ ...job, phase: "published" });
      return released;
    },
    version_yank: (args) => {
      requireAdmin(args.serverId);
      const v = versionById(args.versionId);
      if (v.state !== "published")
        fail({ kind: "conflict", state: v.state } satisfies VersionActionError);
      const yanked: PublishVersion = {
        ...v,
        state: "yanked",
        is_current_release: false,
        yanked_at: new Date().toISOString(),
      };
      replaceVersion(yanked);
      return yanked;
    },
  };
}
