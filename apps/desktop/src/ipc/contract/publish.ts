// Publishing from the launcher (A2-T13, admins only; 02-package-format §3, §6): pick a package and a folder,
// preview the plan, unlock a publisher key file, upload, follow the server's verification, publish or
// yank. The Rust core does all the work: it reads the folder, decrypts the key (zeroized after signing;
// the passphrase is sent once, with `publish_start` or `publish_resume`) and runs `vgames-transfer`'s
// publish flow. The UI never sees file contents or key material. Requested shapes; see core.ts.
import type { AppError, Platform } from "./core";
import { call, makeEvents, type Result } from "./runtime";

export type PublishPackage = { id: string; slug: string; title: string };

export type PublishPackageError =
  /** The account isn't an admin on this server. */
  | { kind: "forbidden" }
  | { kind: "invalid_title" }
  | { kind: "slug_taken"; slug: string }
  | { kind: "offline" }
  | { kind: "server"; code: string };

/** A folder chosen with the native dialog. */
export type PickedFolder = { path: string; name: string };

/** A file that can't be published (02-package-format §3), with the rule it breaks. */
export type InvalidPath = {
  path: string;
  reason:
    | "reserved_name"
    | "too_long"
    | "invalid_characters"
    | "case_collision"
    | "outside_folder"
    | "symlink"
    | "special_file"
    | "unreadable";
};

export type PublishPlan = {
  /** Identifies the scan; `publish_start` refuses when the folder changed since (`plan_changed`). */
  plan_id: string;
  folder: string;
  file_count: number;
  total_bytes: number;
  pack_count: number;
  /** Publishing is blocked while this isn't empty. The core lists at most 500 and says how many more. */
  invalid: InvalidPath[];
  invalid_total: number;
  /** Paths that look like programs (`.exe`, `.sh`, `.app`…), for the start target. */
  executables: string[];
};

export type PublishPlanError =
  | { kind: "not_a_folder" }
  | { kind: "empty_folder" }
  | { kind: "too_many_files"; limit: number }
  | { kind: "io"; detail: string };

/** A key file chosen with the native dialog; its contents stay in Rust. */
export type PickedKey = { key_id: string; file_name: string };

export type PublishStartRequest = {
  plan_id: string;
  package_id: string;
  platform: Platform;
  version_label: string;
  /** Path inside the folder of the program to start; null = nothing to start. */
  executable: string | null;
  arguments: string[];
  key_id: string;
  passphrase: string;
};

export type PublishStartError =
  | { kind: "forbidden" }
  | { kind: "wrong_passphrase" }
  | { kind: "key_unreadable" }
  /** The key isn't in the server's trust bundle, or was revoked or expired (01-security). Nothing was uploaded. */
  | { kind: "untrusted_key" }
  | { kind: "invalid_label" }
  | { kind: "label_taken" }
  /** Files changed after the preview. Preview the folder again. */
  | { kind: "plan_changed" }
  | { kind: "busy" }
  | { kind: "offline" }
  | { kind: "io"; detail: string };

export type PublishPhase =
  | "preparing"
  | "uploading"
  | "signing"
  | "uploading_manifest"
  | "finalizing"
  | "verifying"
  /** Verified by the server and waiting for the admin to publish. */
  | "ready"
  | "publishing"
  | "published"
  | "failed"
  | "cancelled";

export type PublishFailure =
  | { kind: "wrong_passphrase" }
  | { kind: "untrusted_key" }
  /** The server's verification found problems; nothing is published. */
  | { kind: "verification_failed"; detail: string }
  /** The server refused the finalize (02 §6 codes), e.g. `pack_missing`, `manifest_invalid`. */
  | { kind: "finalize_rejected"; code: string }
  | { kind: "offline"; retryable: boolean }
  | { kind: "forbidden" }
  | { kind: "io"; detail: string };

export type PackProgress = { index: number; bytes_done: number; bytes_total: number };

/** `publish-progress`: the whole state of a job, sent when anything changes (at most ~5 per second). */
export type PublishProgress = {
  job_id: string;
  package_id: string;
  version_label: string;
  phase: PublishPhase;
  bytes_done: number;
  bytes_total: number;
  /** Packs being uploaded right now (at most the worker count). */
  packs: PackProgress[];
  packs_done: number;
  pack_count: number;
  /** 0..1 while the server verifies, else null. */
  verification: number | null;
  failure: PublishFailure | null;
};

export type PublishJobError =
  | { kind: "not_found" }
  | { kind: "wrong_passphrase" }
  | { kind: "untrusted_key" }
  | { kind: "plan_changed" }
  | { kind: "forbidden" }
  | { kind: "offline" }
  | { kind: "io"; detail: string };

export type PublishVersionState =
  | "uploading"
  | "verifying"
  | "ready"
  | "published"
  | "yanked"
  | "failed"
  | "aborted";

export type PublishVersion = {
  id: string;
  label: string;
  sequence: number;
  state: PublishVersionState;
  platform: Platform;
  size_bytes: number;
  created_at: string;
  /** The release players get. */
  current: boolean;
};

export type YankError =
  | { kind: "forbidden" }
  | { kind: "not_found" }
  | { kind: "already_yanked" }
  | { kind: "invalid_reason" }
  | { kind: "offline" }
  | { kind: "server"; code: string };

export const publishCommands = {
  /** Packages on the active server, newest first; `query` filters by title. */
  async publishPackages(
    query: string | null,
  ): Promise<Result<PublishPackage[], PublishPackageError>> {
    return call("publish_packages", { query });
  },
  async publishPackageCreate(title: string): Promise<Result<PublishPackage, PublishPackageError>> {
    return call("publish_package_create", { title });
  },
  async publishPickFolder(): Promise<Result<PickedFolder | null, AppError>> {
    return call("publish_pick_folder");
  },
  async publishPlan(folder: string): Promise<Result<PublishPlan, PublishPlanError>> {
    return call("publish_plan", { folder });
  },
  async publishPickKey(): Promise<Result<PickedKey | null, AppError>> {
    return call("publish_pick_key");
  },
  async publishStart(
    request: PublishStartRequest,
  ): Promise<Result<{ job_id: string }, PublishStartError>> {
    return call("publish_start", { request });
  },
  /** Jobs that stopped before the end (cancelled, failed, or the launcher closed), resumable from their records. */
  async publishJobs(): Promise<Result<PublishProgress[], AppError>> {
    return call("publish_jobs");
  },
  /** Stops after the pieces in flight; the upload stays resumable. */
  async publishCancel(jobId: string): Promise<Result<null, PublishJobError>> {
    return call("publish_cancel", { jobId });
  },
  /** Continues an upload. The passphrase is needed while the version is still uploading. */
  async publishResume(jobId: string, passphrase: string): Promise<Result<null, PublishJobError>> {
    return call("publish_resume", { jobId, passphrase });
  },
  /** Makes a verified version the current release. */
  async publishPublish(jobId: string): Promise<Result<null, PublishJobError>> {
    return call("publish_publish", { jobId });
  },
  /** Gives up: the server aborts the unpublished version and the record is deleted. */
  async publishAbort(jobId: string): Promise<Result<null, PublishJobError>> {
    return call("publish_abort", { jobId });
  },
  async publishVersions(packageId: string): Promise<Result<PublishVersion[], PublishPackageError>> {
    return call("publish_versions", { packageId });
  },
  /** Withdraws a published version; players on it are told to update. The reason is 3–500 characters. */
  async publishYank(versionId: string, reason: string): Promise<Result<null, YankError>> {
    return call("publish_yank", { versionId, reason });
  },
};

export const publishEvents = makeEvents<{ publishProgress: PublishProgress }>({
  publishProgress: "publish-progress",
});
