// Pure helpers for the Publish screen: the version name rule and every error's sentence.
import { t } from "../../i18n";
import type {
  PublishFailure,
  PublishJobError,
  PublishPackageError,
  PublishPlanError,
  PublishProgress,
  PublishStartError,
  YankError,
} from "../../ipc";

/** The same rule the server applies to a version's label (02-package-format §6). */
export const LABEL_PATTERN = /^[0-9A-Za-z][0-9A-Za-z._+-]{0,63}$/;

export const isValidLabel = (label: string) => LABEL_PATTERN.test(label);

export const YANK_REASON_MIN = 3;
export const YANK_REASON_MAX = 500;

/** A job that can still change: nothing to show but progress. */
export const isRunning = (p: PublishProgress) =>
  [
    "preparing",
    "uploading",
    "signing",
    "uploading_manifest",
    "finalizing",
    "verifying",
    "publishing",
  ].includes(p.phase);

export function planErrorText(error: PublishPlanError): string {
  switch (error.kind) {
    case "not_a_folder":
      return t("publish.folder.errors.notAFolder");
    case "empty_folder":
      return t("publish.folder.errors.empty");
    case "too_many_files":
      return t("publish.folder.errors.tooMany", { limit: error.limit });
    case "io":
      return t("publish.folder.errors.io", { detail: error.detail });
  }
}

export function packageErrorText(error: PublishPackageError): string {
  switch (error.kind) {
    case "forbidden":
      return t("publish.notAdmin");
    case "invalid_title":
      return t("publish.package.invalidTitle");
    case "slug_taken":
      return t("publish.package.slugTaken", { slug: error.slug });
    case "offline":
      return t("publish.startFailed.offline");
    case "server":
      return t("publish.versions.errors.server", { code: error.code });
  }
}

/** Errors of starting; `field` says which input they belong to. */
export function startError(error: PublishStartError): {
  field: "key" | "passphrase" | "label" | null;
  text: string;
} {
  switch (error.kind) {
    case "wrong_passphrase":
      return { field: "passphrase", text: t("publish.key.wrongPassphrase") };
    case "key_unreadable":
      return { field: "key", text: t("publish.key.unreadable") };
    case "untrusted_key":
      return { field: "key", text: t("publish.key.untrusted") };
    case "invalid_label":
      return { field: "label", text: t("publish.release.invalidLabel") };
    case "label_taken":
      return { field: "label", text: t("publish.release.labelTaken") };
    case "forbidden":
      return { field: null, text: t("publish.startFailed.forbidden") };
    case "plan_changed":
      return { field: null, text: t("publish.startFailed.planChanged") };
    case "busy":
      return { field: null, text: t("publish.startFailed.busy") };
    case "offline":
      return { field: null, text: t("publish.startFailed.offline") };
    case "io":
      return { field: null, text: t("publish.startFailed.io", { detail: error.detail }) };
  }
}

export function failureText(failure: PublishFailure): string {
  switch (failure.kind) {
    case "wrong_passphrase":
      return t("publish.job.failed.wrongPassphrase");
    case "untrusted_key":
      return t("publish.job.failed.untrusted");
    case "verification_failed":
      return t("publish.job.failed.verification", { detail: failure.detail });
    case "finalize_rejected":
      return t("publish.job.failed.finalize", { code: failure.code });
    case "offline":
      return t(
        failure.retryable ? "publish.job.failed.offline" : "publish.job.failed.offlineFinal",
      );
    case "forbidden":
      return t("publish.job.failed.forbidden");
    case "io":
      return t("publish.job.failed.io", { detail: failure.detail });
  }
}

/** What a failed job can still do. Security and verification failures are never retried. */
export function canContinue(failure: PublishFailure): boolean {
  switch (failure.kind) {
    case "untrusted_key":
    case "verification_failed":
    case "forbidden":
      return false;
    default:
      return true;
  }
}

export function jobErrorText(error: PublishJobError): string {
  switch (error.kind) {
    case "wrong_passphrase":
      return t("publish.job.actionFailed.wrongPassphrase");
    case "untrusted_key":
      return t("publish.job.actionFailed.untrusted");
    case "plan_changed":
      return t("publish.job.actionFailed.planChanged");
    case "not_found":
      return t("publish.job.actionFailed.notFound");
    case "forbidden":
      return t("publish.job.actionFailed.forbidden");
    case "offline":
      return t("publish.job.actionFailed.offline");
    case "io":
      return t("publish.job.actionFailed.io", { detail: error.detail });
  }
}

export function yankErrorText(error: YankError): string {
  switch (error.kind) {
    case "invalid_reason":
      return t("publish.versions.errors.invalidReason");
    case "already_yanked":
      return t("publish.versions.errors.alreadyYanked");
    case "not_found":
      return t("publish.versions.errors.notFound");
    case "forbidden":
      return t("publish.versions.errors.forbidden");
    case "offline":
      return t("publish.versions.errors.offline");
    case "server":
      return t("publish.versions.errors.server", { code: error.code });
  }
}
