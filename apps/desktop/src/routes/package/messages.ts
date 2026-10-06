// Typed catalog and install errors as plain sentences.
import { formatBytes, t } from "../../i18n";
import type { CompatBlocker, InstallStartError } from "../../ipc";

export function blockerText(blocker: CompatBlocker): string {
  switch (blocker.kind) {
    case "d3d12_unsupported_on_mac":
      return t("compat.needsAppleSilicon");
    case "needs_rosetta":
      return t("compat.needsRosetta");
    case "rosetta_sunset":
      return t("compat.rosettaSunset", { version: blocker.last_macos });
  }
}

/** Blockers that stop an install (the Rosetta sunset is only a warning). */
export function isHardBlocker(blocker: CompatBlocker): boolean {
  return blocker.kind === "d3d12_unsupported_on_mac" || blocker.kind === "needs_rosetta";
}

export function installErrorMessage(error: InstallStartError, title: string): string {
  switch (error.kind) {
    case "not_found":
      return t("install.errors.notFound");
    case "no_release":
      return t("install.errors.noRelease", { title });
    case "already_installed":
      return t("install.errors.alreadyInstalled", { title });
    case "offline":
      return t("install.errors.offline");
    case "blocked":
      return t("install.errors.blocked", { title, reason: blockerText(error.blocker) });
    case "server":
      return t("install.errors.server", { code: error.code });
    case "insufficient_space":
      return t("install.errors.insufficientSpace", {
        required: formatBytes(error.required_bytes),
        available: formatBytes(error.available_bytes),
      });
    case "library_offline":
      return t("install.errors.libraryOffline", { path: error.library_path });
    case "trust_expired":
      return t("install.errors.trustExpired");
    case "io":
      return t("install.errors.io", { detail: error.detail });
  }
}
