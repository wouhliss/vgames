// Text and rules for the publish screen; no React here.
import { formatBytes, formatPercent, formatRate, t } from "../../i18n";
import type {
  KeyError,
  Platform,
  PublishCommandError,
  PublishJob,
  PublishJobError,
  PublishPhase,
  Role,
} from "../../ipc";

export const PLATFORMS: readonly { value: Platform; label: string }[] = [
  { value: "windows-x86_64", label: "Windows (x64)" },
  { value: "windows-aarch64", label: "Windows (Arm)" },
  { value: "linux-x86_64", label: "Linux (x64)" },
  { value: "linux-aarch64", label: "Linux (Arm)" },
  { value: "macos-aarch64", label: "macOS (Apple silicon)" },
  { value: "macos-x86_64", label: "macOS (Intel)" },
];

export function platformLabel(platform: Platform): string {
  return PLATFORMS.find((p) => p.value === platform)?.label ?? platform;
}

/** Admins and owners publish; the Rust core checks the role again on every command. */
export function canPublish(role: Role | null | undefined): boolean {
  return role === "admin" || role === "owner";
}

export function keyErrorText(error: KeyError): string {
  switch (error.kind) {
    case "wrong_passphrase":
      return t("publish.keyError.wrong_passphrase");
    case "invalid_key_file":
      return t("publish.keyError.invalid_key_file");
    case "untrusted_key":
      return t(`publish.keyError.${error.reason}`);
  }
}

export function commandErrorText(error: PublishCommandError): string {
  switch (error.kind) {
    case "key":
      return keyErrorText(error.error);
    case "server":
      return t("publish.error.server", { message: error.message });
    case "network":
      return t("publish.error.network");
    case "invalid_field":
      return error.message;
    case "version_conflict":
      return t("publish.error.version_conflict", { state: t(`publish.state.${error.state}`) });
    case "invalid_paths":
      return t("publish.error.invalid_paths", { count: error.count });
    case "io":
      return t("publish.error.io", { detail: error.detail });
    case "forbidden":
    case "unauthenticated":
    case "not_found":
    case "slug_taken":
    case "job_conflict":
    case "empty_folder":
    case "invalid_label":
    case "invalid_launch":
    case "key_required":
    case "internal":
      return t(`publish.error.${error.kind}`);
  }
}

export function jobErrorText(error: PublishJobError): string {
  switch (error.kind) {
    case "verification_failed":
      return error.reason
        ? t("publish.jobError.verification_reason", { reason: error.reason })
        : t("publish.jobError.verification_failed");
    case "source_changed":
      return t("publish.jobError.source_changed");
    case "remote":
      return error.retryable ? t("publish.jobError.remoteRetry") : t("publish.jobError.remote");
    case "version_gone":
      return t("publish.jobError.version_gone", { state: t(`publish.state.${error.state}`) });
    case "io":
      return t("publish.jobError.io", { detail: error.detail });
    case "internal":
      return t("publish.jobError.internal");
  }
}

const RUNNING: readonly PublishPhase[] = [
  "preparing",
  "uploading",
  "signing",
  "uploading_manifest",
  "finalizing",
  "verifying",
  "publishing",
];

export function isRunning(job: PublishJob): boolean {
  return RUNNING.includes(job.phase);
}

/** Resumable: stopped, or failed on something a retry may fix. */
export function canResume(job: PublishJob): boolean {
  if (job.phase === "cancelled") return true;
  if (job.phase !== "failed" || !job.error) return false;
  return job.error.kind === "remote" || job.error.kind === "io" || job.error.kind === "internal";
}

/** The line under a job's title: phase, then bytes or verification progress. */
export function jobStatus(job: PublishJob): string {
  const phase = t(`publish.phase.${job.phase}`);
  if (job.phase === "verifying" && job.verification !== null)
    return `${phase} · ${t("publish.verification", { percent: formatPercent(job.verification) })}`;
  if (job.phase === "uploading" || job.phase === "cancelled") {
    const bytes = t("publish.progress", {
      done: formatBytes(job.bytes_confirmed),
      total: formatBytes(job.bytes_total),
    });
    const speed = job.bytes_per_second ?? 0;
    const rate = job.phase === "uploading" && speed > 0 ? ` · ${formatRate(speed)}` : "";
    return `${phase} · ${bytes}${rate}`;
  }
  return phase;
}

/** 0–1 for the job's bar: upload bytes, then verification. */
export function jobProgress(job: PublishJob): number | null {
  if (job.phase === "verifying") return job.verification;
  if (job.bytes_total <= 0) return null;
  return Math.min(1, job.bytes_confirmed / job.bytes_total);
}

export function validLabel(label: string): boolean {
  const trimmed = label.trim();
  return trimmed.length > 0 && [...trimmed].length <= 64;
}
