// User-facing text for every error the onboarding commands can return.
import { t } from "../../i18n";
import type { AuthError, LibraryError, ServerError } from "../../ipc";

export function serverErrorMessage(error: ServerError): string {
  switch (error.kind) {
    case "invalid_url":
      return t("onboarding.errors.invalidUrl");
    case "insecure_scheme":
      return t("onboarding.errors.insecureScheme");
    case "unreachable":
      return t("onboarding.errors.unreachable");
    case "timeout":
      return t("onboarding.errors.timeout");
    case "tls":
      return t("onboarding.errors.tls");
    case "not_vgames":
      return t("onboarding.errors.notVgames");
    case "already_added":
      return t("onboarding.errors.alreadyAdded");
    case "preview_expired":
      return t("onboarding.errors.previewExpired");
    case "launcher_too_old":
      return t("onboarding.tooOld.text", {
        min: error.min_version,
        current: error.current_version,
      });
    case "fingerprint_mismatch":
      return t("onboarding.linkMismatch.text");
  }
}

export function authErrorMessage(error: AuthError): string {
  switch (error.kind) {
    case "registration_closed":
      return t("onboarding.signIn.errors.registrationClosed");
    case "not_allowlisted":
      return t("onboarding.signIn.errors.notAllowlisted");
    case "user_disabled":
      return t("onboarding.signIn.errors.userDisabled");
    case "expired":
      return t("onboarding.signIn.errors.expired");
    case "invalid_code":
      return t("onboarding.signIn.errors.invalidCode");
    case "cancelled":
      return t("onboarding.signIn.errors.cancelled");
    case "browser_unavailable":
      return t("onboarding.signIn.errors.browserUnavailable");
    case "network":
      return t("onboarding.signIn.errors.network");
    case "server":
      return t("onboarding.signIn.errors.server", { code: error.code });
  }
}

export function libraryErrorMessage(error: LibraryError): string {
  switch (error.kind) {
    case "not_writable":
      return t("onboarding.library.errors.notWritable");
    case "system_directory":
      return t("onboarding.library.errors.systemDirectory");
    case "nested_in_library":
      return t("onboarding.library.errors.nestedInLibrary", { path: error.library_path });
    case "contains_library":
      return t("onboarding.library.errors.containsLibrary", { path: error.library_path });
    case "already_added":
      return t("onboarding.library.errors.alreadyAdded");
    case "not_found":
      return t("onboarding.library.errors.notFound");
    case "io":
      return t("onboarding.library.errors.io", { detail: error.detail });
  }
}
