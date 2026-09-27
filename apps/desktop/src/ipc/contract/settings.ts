// Settings that are not covered by another domain: account sessions and credential storage, library
// management, download limits, the overlay's per-package switches, privacy and overlay preferences
// (Agent 4's `SocialSettings`, 05-social-notes §5) and third-party licenses (Agents 2 and 4).
// Requested shapes; see core.ts for the conventions.
import type { PackageRef, SocialError } from "../../bindings";
import type { AppError, LibraryError } from "./core";
import { call, get, type Result } from "./runtime";

export type SessionPlatform = "windows" | "linux" | "macos" | "web" | "cli";

/** A signed-in device of this account on this server (`GET /v1/me/sessions`). */
export type AccountSession = {
  id: string;
  /** User agent summary the server stored, e.g. "vgames 0.4 on Windows". */
  device_name: string;
  platform: SessionPlatform;
  created_at: string;
  last_used_at: string | null;
  /** This launcher's own session. */
  current: boolean;
};

/**
 * Where the launcher keeps refresh tokens (01-security §7). `file_fallback`: no OS keychain was
 * available, so tokens sit in a file readable by this user; the UI warns about it.
 */
export type CredentialStorage = { kind: "keychain" } | { kind: "file_fallback"; path: string };

export type DownloadSettings = {
  /** Kibibytes per second; null = unlimited (02-package-format §7.12). */
  bandwidth_limit_kib: number | null;
  /** How many installs run at the same time, 1–3. */
  concurrent_installs: number;
};

export type SettingsError =
  | { kind: "invalid"; field: string; detail: string }
  | { kind: "io"; detail: string };

// `social_settings_get|set` and `SocialSettings` (05-social-notes §5) are generated now; the typed
// hotkey errors below are still requested as `SocialError` variants (A4-T10 registers the hotkey).

export type HotkeyError =
  /** Not a valid accelerator (no key, or a modifier alone). */
  | { kind: "invalid" }
  /** The operating system or another application already uses it. */
  | { kind: "in_use"; by: string | null };

/** What saving social settings can report: the generated errors plus the requested hotkey ones. */
export type SocialSettingsError = SocialError | HotkeyError;

/** The per-package "In-game overlay" switch and its crash safety valve (05-social §6.3). */
export type PackageOverlay = {
  package: PackageRef;
  title: string;
  enabled: boolean;
  /** Set when the launcher turned the overlay off after two quick abnormal exits. */
  disabled_by_safety_valve_at: string | null;
};

export type LibraryRemoveError =
  | LibraryError
  /** Installs still live there: move or uninstall them first. */
  | { kind: "not_empty"; install_count: number }
  /** The default library can't be removed; make another one the default first. */
  | { kind: "is_default" };

export const settingsCommands = {
  async accountSessions(serverId: string): Promise<Result<AccountSession[], AppError>> {
    return call("account_sessions", { serverId });
  },
  async accountSessionRevoke(serverId: string, sessionId: string): Promise<Result<null, AppError>> {
    return call("account_session_revoke", { serverId, sessionId });
  },
  async credentialStorage(): Promise<CredentialStorage> {
    return get("credential_storage");
  },

  async librarySetDefault(libraryId: string): Promise<Result<null, LibraryError>> {
    return call("library_set_default", { libraryId });
  },
  /** Forgets a library. Files on disk are left alone. */
  async libraryRemove(libraryId: string): Promise<Result<null, LibraryRemoveError>> {
    return call("library_remove", { libraryId });
  },

  async downloadSettingsGet(): Promise<DownloadSettings> {
    return get("download_settings_get");
  },
  async downloadSettingsSet(
    settings: DownloadSettings,
  ): Promise<Result<DownloadSettings, SettingsError>> {
    return call("download_settings_set", { settings });
  },

  async overlayPackages(): Promise<Result<PackageOverlay[], AppError>> {
    return call("overlay_packages");
  },
  /** Turning it on also clears the safety valve. */
  async overlayPackageSet(pkg: PackageRef, enabled: boolean): Promise<Result<null, AppError>> {
    return call("overlay_package_set", { package: pkg, enabled });
  },

  /** Third-party notices bundled with the launcher (plain text). */
  async appLicenses(): Promise<Result<string, AppError>> {
    return call("app_licenses");
  },
};
