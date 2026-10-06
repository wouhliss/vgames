// Every typed error from the library commands mapped to a plain sentence.
import { formatBytes, t } from "../../i18n";
import type { CollectionError, InstallActionError, LaunchError } from "../../ipc";

export function launchErrorMessage(error: LaunchError, title: string): string {
  switch (error.kind) {
    case "not_installed":
      return t("library.launchErrors.notInstalled", { title });
    case "incomplete":
      return t("library.launchErrors.incomplete", { title });
    case "busy":
      return t("library.launchErrors.busy", { title });
    case "library_offline":
      return t("library.launchErrors.libraryOffline", { title, path: error.library_path });
    case "already_running":
      return t("library.launchErrors.alreadyRunning", { title });
    case "target_not_found":
      return t("library.launchErrors.targetNotFound", { title });
    case "integrity":
      return t("library.launchErrors.integrity", { title, path: error.path });
    case "key_revoked":
      return t("library.launchErrors.keyRevoked", { title });
    case "compat_unavailable":
      return t("library.launchErrors.compatUnavailable", { title, detail: error.detail });
    case "rate_limited":
      return t("library.launchErrors.rateLimited");
    case "save_conflict":
      return t("library.launchErrors.saveConflict", { title });
    case "io":
      return t("library.launchErrors.io", { title, detail: error.detail });
  }
}

export function actionErrorMessage(error: InstallActionError, title: string): string {
  switch (error.kind) {
    case "not_found":
      return t("library.actionErrors.notFound", { title });
    case "busy":
      return t("library.actionErrors.busy", { title });
    case "running":
      return t("library.actionErrors.running", { title });
    case "library_offline":
      return t("library.actionErrors.libraryOffline", { path: error.library_path });
    case "offline":
      return t("library.actionErrors.offline");
    case "insufficient_space":
      return t("library.actionErrors.insufficientSpace", {
        required: formatBytes(error.required_bytes),
        available: formatBytes(error.available_bytes),
      });
    case "same_library":
      return t("library.actionErrors.sameLibrary", { title });
    case "io":
      return t("library.actionErrors.io", { detail: error.detail });
  }
}

export function collectionErrorMessage(error: CollectionError): string {
  switch (error.kind) {
    case "invalid_name":
      return t("library.collections.errors.invalidName");
    case "name_taken":
      return t("library.collections.errors.nameTaken");
    case "not_found":
      return t("library.collections.errors.notFound");
    case "io":
      return t("library.collections.errors.io", { detail: error.detail });
  }
}
