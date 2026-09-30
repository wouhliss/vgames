// Admin publishing (A2-T13): packages, the folder plan, key unlock, a simulated upload and the
// server's verification, publish and yank. `scenario` picks how a run ends; with `auto` off a test
// drives the job by emitting `publish-progress` itself.
import type {
  InstalledPackage,
  PublishFailure,
  PublishJobError,
  PublishPackage,
  PublishPackageError,
  PublishPlan,
  PublishPlanError,
  PublishProgress,
  PublishStartError,
  PublishStartRequest,
  PublishVersion,
  YankError,
} from "../ipc";
import { events } from "../ipc";
import { fail, type Handler, later, mockId } from "./runtime";

export type PublishScenario = "ok" | "verification_failed" | "finalize_rejected" | "offline";

export const CORRECT_PASSPHRASE = "correct horse battery";

export interface PublishState {
  publish: {
    packages: PublishPackage[];
    versions: Record<string, PublishVersion[]>;
    /** What `publish_plan` returns for a folder path (otherwise `DEFAULT_PLAN`). */
    plans: Record<string, PublishPlan>;
    pickedFolder: { path: string; name: string } | null;
    pickedKey: { key_id: string; file_name: string } | null;
    keyTrusted: boolean;
    scenario: PublishScenario;
    /** Run the upload by itself (timers, or at once when `stepMs` is 0). */
    auto: boolean;
    stepMs: number;
    /** Changes the folder after the preview (`plan_changed`). */
    folderChanged: boolean;
    jobs: Record<string, PublishProgress & { request: PublishStartRequest }>;
    errors: Record<
      string,
      PublishPackageError | PublishPlanError | PublishStartError | PublishJobError | YankError
    >;
    /** Yank requests, for assertions. */
    yanked: { version_id: string; reason: string }[];
  };
}

const GIB = 1024 ** 3;

export const DEFAULT_PLAN: PublishPlan = {
  plan_id: "plan-1",
  folder: "/home/sam/builds/hollow-harbor",
  file_count: 1204,
  total_bytes: 5 * GIB,
  pack_count: 6,
  invalid: [],
  invalid_total: 0,
  executables: ["HollowHarbor.exe", "tools/Editor.exe"],
};

export function defaultPublishState(): PublishState {
  return {
    publish: {
      packages: [
        { id: mockId("pkg", 1), slug: "hollow-harbor", title: "Hollow Harbor" },
        { id: mockId("pkg", 2), slug: "crimson-canyon", title: "Crimson Canyon" },
      ],
      versions: {
        [mockId("pkg", 1)]: [
          {
            id: mockId("ver", 2),
            label: "1.1.0",
            sequence: 2,
            state: "published",
            platform: "windows-x86_64",
            size_bytes: 5 * GIB,
            created_at: "2026-09-20T10:00:00Z",
            current: true,
          },
          {
            id: mockId("ver", 1),
            label: "1.0.0",
            sequence: 1,
            state: "published",
            platform: "windows-x86_64",
            size_bytes: 4 * GIB,
            created_at: "2026-09-01T10:00:00Z",
            current: false,
          },
        ],
      },
      plans: {},
      pickedFolder: { path: DEFAULT_PLAN.folder, name: "hollow-harbor" },
      pickedKey: { key_id: "key-1", file_name: "studio.vgkey" },
      keyTrusted: true,
      scenario: "ok",
      auto: true,
      stepMs: 0,
      folderChanged: false,
      jobs: {},
      errors: {},
      yanked: [],
    },
  };
}

export function publishHandlers(
  state: PublishState & { installs: InstalledPackage[] },
): Record<string, Handler> {
  const p = state.publish;
  const forced = (cmd: string) => {
    const error = p.errors[cmd];
    if (error) fail(error);
  };
  let seq = 0;

  const emit = (
    job: PublishProgress & { request: PublishStartRequest },
    patch: Partial<PublishProgress>,
  ) => {
    Object.assign(job, patch);
    const { request: _request, ...progress } = job;
    void events.publishProgress.emit(progress);
  };

  const failure = (): PublishFailure | null => {
    switch (p.scenario) {
      case "verification_failed":
        return {
          kind: "verification_failed",
          detail: "file 'data/level3.pak' doesn't match its hash",
        };
      case "finalize_rejected":
        return { kind: "finalize_rejected", code: "pack_missing" };
      case "offline":
        return { kind: "offline", retryable: true };
      default:
        return null;
    }
  };

  /** Runs the job to its end. Every step is an event, so the UI only ever follows events. */
  const run = (job: PublishProgress & { request: PublishStartRequest }) => {
    const total = job.bytes_total;
    const steps: (() => void)[] = [
      () =>
        emit(job, {
          phase: "uploading",
          bytes_done: Math.round(total / 2),
          packs_done: 3,
          packs: [
            { index: 3, bytes_done: 100, bytes_total: 200 },
            { index: 4, bytes_done: 40, bytes_total: 200 },
          ],
        }),
      () =>
        emit(job, { phase: "signing", bytes_done: total, packs_done: job.pack_count, packs: [] }),
      () => emit(job, { phase: "uploading_manifest" }),
      () => emit(job, { phase: "finalizing" }),
    ];
    const bad = failure();
    if (bad && bad.kind !== "verification_failed") {
      steps.push(() => emit(job, { phase: "failed", failure: bad }));
    } else {
      steps.push(() => emit(job, { phase: "verifying", verification: 0.4 }));
      steps.push(() =>
        bad
          ? emit(job, { phase: "failed", verification: null, failure: bad })
          : emit(job, { phase: "ready", verification: null }),
      );
    }
    for (const [i, step] of steps.entries()) later(p.stepMs * (i + 1), step);
  };

  return {
    publish_packages: (args) => {
      forced("publish_packages");
      const q = String(args.query ?? "").toLowerCase();
      return p.packages.filter((x) => x.title.toLowerCase().includes(q));
    },
    publish_package_create: (args) => {
      forced("publish_package_create");
      const title = String(args.title).trim();
      if ([...title].length < 1 || [...title].length > 200)
        fail({ kind: "invalid_title" } satisfies PublishPackageError);
      const slug = title
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, "-")
        .replace(/^-|-$/g, "");
      if (p.packages.some((x) => x.slug === slug))
        fail({ kind: "slug_taken", slug } satisfies PublishPackageError);
      const created = { id: mockId("pkg", 100 + p.packages.length), slug, title };
      p.packages = [created, ...p.packages];
      return created;
    },
    publish_pick_folder: () => p.pickedFolder,
    publish_plan: (args) => {
      forced("publish_plan");
      const folder = String(args.folder);
      return p.plans[folder] ?? { ...DEFAULT_PLAN, folder };
    },
    publish_pick_key: () => p.pickedKey,
    publish_start: (args) => {
      forced("publish_start");
      const request = args.request as PublishStartRequest;
      if (p.folderChanged) fail({ kind: "plan_changed" } satisfies PublishStartError);
      if (!/^[0-9A-Za-z][0-9A-Za-z._+-]{0,63}$/.test(request.version_label))
        fail({ kind: "invalid_label" } satisfies PublishStartError);
      const existing = p.versions[request.package_id] ?? [];
      if (existing.some((v) => v.label === request.version_label))
        fail({ kind: "label_taken" } satisfies PublishStartError);
      if (request.passphrase !== CORRECT_PASSPHRASE)
        fail({ kind: "wrong_passphrase" } satisfies PublishStartError);
      if (!p.keyTrusted) fail({ kind: "untrusted_key" } satisfies PublishStartError);
      seq += 1;
      const job = {
        job_id: mockId("job", seq),
        package_id: request.package_id,
        version_label: request.version_label,
        phase: "preparing" as const,
        bytes_done: 0,
        bytes_total: 5 * GIB,
        packs: [],
        packs_done: 0,
        pack_count: 6,
        verification: null,
        failure: null,
        request,
      };
      p.jobs[job.job_id] = job;
      emit(job, {});
      if (p.auto) run(job);
      return { job_id: job.job_id };
    },
    publish_jobs: () =>
      Object.values(p.jobs)
        .filter((j) => j.phase === "cancelled" || j.phase === "failed")
        .map(({ request: _r, ...progress }) => progress),
    publish_cancel: (args) => {
      forced("publish_cancel");
      const job = p.jobs[String(args.jobId)];
      if (!job) fail({ kind: "not_found" } satisfies PublishJobError);
      emit(job, { phase: "cancelled", packs: [] });
      return null;
    },
    publish_resume: (args) => {
      forced("publish_resume");
      const job = p.jobs[String(args.jobId)];
      if (!job) fail({ kind: "not_found" } satisfies PublishJobError);
      if (args.passphrase !== CORRECT_PASSPHRASE)
        fail({ kind: "wrong_passphrase" } satisfies PublishJobError);
      emit(job, { phase: "uploading", failure: null });
      if (p.auto) run(job);
      return null;
    },
    publish_publish: (args) => {
      forced("publish_publish");
      const job = p.jobs[String(args.jobId)];
      if (!job) fail({ kind: "not_found" } satisfies PublishJobError);
      const list = p.versions[job.package_id] ?? [];
      p.versions[job.package_id] = [
        {
          id: mockId("ver", 50 + list.length),
          label: job.version_label,
          sequence: list.length + 1,
          state: "published",
          platform: job.request.platform,
          size_bytes: job.bytes_total,
          created_at: "2026-09-30T12:00:00Z",
          current: true,
        },
        ...list.map((v) => ({ ...v, current: false })),
      ];
      emit(job, { phase: "published" });
      return null;
    },
    publish_abort: (args) => {
      forced("publish_abort");
      const id = String(args.jobId);
      if (!p.jobs[id]) fail({ kind: "not_found" } satisfies PublishJobError);
      delete p.jobs[id];
      return null;
    },
    publish_versions: (args) => {
      forced("publish_versions");
      return p.versions[String(args.packageId)] ?? [];
    },
    publish_yank: (args) => {
      forced("publish_yank");
      const reason = String(args.reason).trim();
      if (reason.length < 3 || reason.length > 500)
        fail({ kind: "invalid_reason" } satisfies YankError);
      const id = String(args.versionId);
      let found = false;
      for (const [pkg, list] of Object.entries(p.versions)) {
        p.versions[pkg] = list.map((v) => {
          if (v.id !== id) return v;
          found = true;
          if (v.state === "yanked") fail({ kind: "already_yanked" } satisfies YankError);
          return { ...v, state: "yanked" as const, current: false };
        });
      }
      if (!found) fail({ kind: "not_found" } satisfies YankError);
      p.yanked.push({ version_id: id, reason });
      return null;
    },
  };
}
