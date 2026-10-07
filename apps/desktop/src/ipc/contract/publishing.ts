// Launcher admin publishing (INS-06; 02-package-format §6, 03-api admin packages and versions).
// Requested shapes; see core.ts for the conventions. Admins and owners only: the Rust core checks the
// signed-in account's role on every command and answers `forbidden` otherwise; the UI hides the route.
//
// Flow: pick a package (or create one) → pick a folder → `publish_plan` → `publish_start` (uploads,
// signs, verifies) → the job stops at `ready` → `publish_release` makes it the current release. The
// key file is read and decrypted in Rust and zeroized after signing; the passphrase is only passed
// through. A job survives a launcher restart: `publish_jobs` lists it again and `publish_resume`
// continues it (it asks for the key again while the manifest is not signed yet).
import type { Platform } from "../../bindings";
import { call, makeEvents, type Result } from "./runtime";

// ------------------------------------------------------------------------------------------------
// Packages and versions on the server

export type PackageStatus = "draft" | "published" | "hidden" | "archived";

/** A package as the admin API returns it (any status). Text is plain text from the server. */
export type PublishPackage = {
  id: string;
  slug: string;
  title: string;
  status: PackageStatus;
  /** Platforms with a current release (from the package's `releases`). */
  released_platforms: Platform[];
};

export type PublishPackagePage = { items: PublishPackage[]; next_cursor: string | null };

export type PackageCreate = {
  title: string;
  /** Derived from the title by the server when null. */
  slug: string | null;
};

export type VersionState =
  | "uploading"
  | "verifying"
  | "ready"
  | "published"
  | "failed"
  | "yanked"
  | "aborted";

export type PublishVersion = {
  id: string;
  package_id: string;
  platform: Platform;
  sequence: number;
  version_label: string;
  state: VersionState;
  is_current_release: boolean;
  /** Set when `state` is `failed` (plain text from the server). */
  failure_reason: string | null;
  total_size: number | null;
  /** 0–1 while `verifying`. */
  verify_progress: number | null;
  created_at: string;
  published_at: string | null;
  yanked_at: string | null;
};

// ------------------------------------------------------------------------------------------------
// The folder to publish

/** Why an entry can't be packed (02-package-format §2, `vgames-core` path rules); any one blocks publishing. */
export type InvalidPathReason =
  | "symlink"
  | "special_file"
  | "not_utf8"
  | "not_nfc"
  /** Empty, absolute, `\\`, or an empty, `.` or `..` component. */
  | "bad_structure"
  | "too_long"
  /** A control character or one of `< > : " | ? *`. */
  | "forbidden_character"
  | "trailing_dot_or_space"
  /** A name Windows reserves (`CON`, `NUL`, `COM1`, …). */
  | "reserved_name"
  /** Inside the `.vgames` folder the launcher keeps for itself. */
  | "reserved_folder"
  | "unsafe_compatibility_form"
  /** Differs from another path only by letter case (`other` names it). */
  | "case_collision"
  | "file_is_folder"
  | "duplicate";

export type InvalidPath = { path: string; reason: InvalidPathReason; other: string | null };

export type PublishPlan = {
  folder: string;
  file_count: number;
  total_bytes: number;
  pack_count: number;
  /** Files that look runnable (`.exe`, `.bat`, `.sh`, `.app` bundles, executable bits), for the launch picker. */
  executables: string[];
  /** Non-empty = publishing is blocked until they are fixed. At most 200, `invalid_count` is the total. */
  invalid_paths: InvalidPath[];
  invalid_count: number;
};

// ------------------------------------------------------------------------------------------------
// Publishing jobs

/** What the game runs when a player presses Play; null = the player picks from the files. */
export type PublishLaunch = {
  /** Relative path inside the folder, `/`-separated; one of `PublishPlan.executables`. */
  executable: string;
  args: string[];
  /** Relative to the folder; null = the executable's folder. */
  working_dir: string | null;
};

export type PublishStart = {
  package_id: string;
  platform: Platform;
  /** 1–64 characters. */
  version_label: string;
  folder: string;
  launch: PublishLaunch | null;
  /** An encrypted publisher key file (`vgames keygen`). */
  key_path: string;
  passphrase: string;
};

export type PublishPhase =
  | "preparing"
  | "uploading"
  | "signing"
  | "uploading_manifest"
  | "finalizing"
  | "verifying"
  /** Verified; waits for `publish_release`. */
  | "ready"
  | "publishing"
  | "published"
  | "failed"
  | "cancelled";

export type PackProgress = {
  index: number;
  bytes_confirmed: number;
  bytes_total: number;
  state: "waiting" | "uploading" | "done";
};

/** Why a publisher key can't sign here. */
export type KeyError =
  /** The passphrase does not decrypt the key file. */
  | { kind: "wrong_passphrase" }
  /** Not a publisher key file, a damaged one, or it can't be read. */
  | { kind: "invalid_key_file" }
  /**
   * The server's trust bundle doesn't let this key sign now: `unknown` (not in it), `revoked`,
   * `other_holder` (another account's key), `not_valid_now` (outside its validity window), or
   * `no_bundle` (the server has no trust bundle yet).
   */
  | {
      kind: "untrusted_key";
      reason: "unknown" | "revoked" | "other_holder" | "not_valid_now" | "no_bundle";
    };

/** Why a job stopped in `failed`. */
export type PublishJobError =
  /** The server could not verify the upload; `reason` is the server's text, if any. */
  | { kind: "verification_failed"; reason: string | null }
  /** The folder changed since the upload started; publish it as a new version. */
  | { kind: "source_changed" }
  /** Network or server trouble; `retryable` = resuming may work. */
  | { kind: "remote"; retryable: boolean }
  /** The version ended on the server (aborted, yanked or failed by someone else). */
  | { kind: "version_gone"; state: VersionState }
  | { kind: "io"; detail: string }
  | { kind: "internal"; detail: string };

export type PublishJob = {
  id: string;
  server_id: string;
  package_id: string;
  package_title: string;
  platform: Platform;
  version_label: string;
  /** Null until the server assigned the version. */
  version_id: string | null;
  phase: PublishPhase;
  bytes_confirmed: number;
  bytes_total: number;
  bytes_per_second: number;
  packs: PackProgress[];
  /** 0–1 while `verifying`. */
  verification: number | null;
  /** Set when `phase` is `failed`. */
  error: PublishJobError | null;
  /** True while resuming needs the key again (the manifest is not signed yet). */
  resume_needs_key: boolean;
};

/** Every publishing command fails with one of these (a job's own failure is `PublishJob.error`). */
export type PublishCommandError =
  /** The signed-in account is not an admin or owner of this server. */
  | { kind: "forbidden" }
  | { kind: "unauthenticated" }
  | { kind: "not_found" }
  | { kind: "network"; detail: string }
  /** Any other refusal from the server (plain text). */
  | { kind: "server"; code: string; message: string }
  /** `publish_package_create`: another package has this slug. */
  | { kind: "slug_taken" }
  /** The server refused a field (`title` or `slug`), with its message (plain text). */
  | { kind: "invalid_field"; field: "title" | "slug"; message: string }
  /** `publish_release` / `version_yank`: the version is not in a state that allows it. */
  | { kind: "version_conflict"; state: VersionState }
  /** `publish_cancel` / `publish_resume` / `publish_dismiss`: not possible in this phase. */
  | { kind: "job_conflict"; phase: PublishPhase }
  /** `publish_plan`: the folder has no files. */
  | { kind: "empty_folder" }
  /** `publish_start`: the version label is empty or over 64 characters. */
  | { kind: "invalid_label" }
  /** `publish_start`: `publish_plan` lists invalid paths; nothing was uploaded. */
  | { kind: "invalid_paths"; count: number }
  /** `publish_start`: the launch executable is not a file in the folder. */
  | { kind: "invalid_launch" }
  /** `publish_resume`: the key is needed again (`resume_needs_key`) and was not given. */
  | { kind: "key_required" }
  /** `publish_start` / `publish_resume` checked the key first. */
  | { kind: "key"; error: KeyError }
  | { kind: "io"; detail: string }
  | { kind: "internal"; detail: string };

export type PublishResume = { key_path: string; passphrase: string };

/** A job's state changed (phase, bytes, a pack or verification). Throttled to 4 per second per job. */
export type PublishProgressEvent = PublishJob;

// ------------------------------------------------------------------------------------------------
// Commands

export const publishingCommands = {
  async publishPackages(
    serverId: string,
    query: string | null,
    cursor: string | null,
  ): Promise<Result<PublishPackagePage, PublishCommandError>> {
    return call("publish_packages", { serverId, query, cursor });
  },
  async publishPackageCreate(
    serverId: string,
    create: PackageCreate,
  ): Promise<Result<PublishPackage, PublishCommandError>> {
    return call("publish_package_create", { serverId, create });
  },
  /** Every version of a package, newest first. */
  async publishVersions(
    serverId: string,
    packageId: string,
  ): Promise<Result<PublishVersion[], PublishCommandError>> {
    return call("publish_versions", { serverId, packageId });
  },
  /** Native folder picker; null when cancelled. */
  async publishPickFolder(): Promise<Result<string | null, PublishCommandError>> {
    return call("publish_pick_folder");
  },
  /** Native file picker for a publisher key file; null when cancelled. */
  async publishPickKey(): Promise<Result<string | null, PublishCommandError>> {
    return call("publish_pick_key");
  },
  /** Scans the folder (no upload). */
  async publishPlan(folder: string): Promise<Result<PublishPlan, PublishCommandError>> {
    return call("publish_plan", { folder });
  },
  /** Checks the key, creates the version and starts uploading. Progress arrives as `publish-progress`. */
  async publishStart(
    serverId: string,
    start: PublishStart,
  ): Promise<Result<PublishJob, PublishCommandError>> {
    return call("publish_start", { serverId, start });
  },
  /** Jobs not yet published or dismissed, including ones from before a restart. */
  async publishJobs(): Promise<Result<PublishJob[], PublishCommandError>> {
    return call("publish_jobs");
  },
  /** Stops uploading; the job keeps what is uploaded and can be resumed. */
  async publishCancel(jobId: string): Promise<Result<PublishJob, PublishCommandError>> {
    return call("publish_cancel", { jobId });
  },
  /** `key` is required when `resume_needs_key`, ignored otherwise. */
  async publishResume(
    jobId: string,
    key: PublishResume | null,
  ): Promise<Result<PublishJob, PublishCommandError>> {
    return call("publish_resume", { jobId, key });
  },
  /** Forgets a finished, failed or cancelled job (aborting its version on the server if unpublished). */
  async publishDismiss(jobId: string): Promise<Result<null, PublishCommandError>> {
    return call("publish_dismiss", { jobId });
  },
  /** Makes a `ready` version the current release of its platform. */
  async publishRelease(
    serverId: string,
    versionId: string,
  ): Promise<Result<PublishVersion, PublishCommandError>> {
    return call("publish_release", { serverId, versionId });
  },
  /** Withdraws a published version; players keep installed copies but can't install it again. */
  async versionYank(
    serverId: string,
    versionId: string,
  ): Promise<Result<PublishVersion, PublishCommandError>> {
    return call("version_yank", { serverId, versionId });
  },
};

export const publishingEvents = makeEvents<{ publishProgress: PublishProgressEvent }>({
  publishProgress: "publish-progress",
});
