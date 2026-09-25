// Commands and events the UI needs that are NOT in the generated `src/bindings.ts` yet.
//
// Same shape as tauri-specta output (`Result`, snake_case payload fields, camelCase argument keys), so
// the UI can be built and tested today against mockIPC. This is the request list for Agent 2 and
// Agent 4 (docs/agents/status/agent-3.md, "Needs from others"). As each item lands in `bindings.ts`,
// delete it here: `src/ipc/index.ts` merges both and the generated one wins, so type errors show drift.
//
// Conventions (tauri-specta defaults):
// - Command names are snake_case on the wire; argument keys are camelCase.
// - Commands returning `Result<T, E>` in Rust resolve to `{ status: "ok", data } | { status: "error", error }`.
// - Rust enums are `#[serde(tag = "kind", rename_all = "snake_case")]`.
// - Event names are the kebab-case type name (`UiNav` → "ui-nav").
import { invoke as TAURI_INVOKE } from "@tauri-apps/api/core";
import * as TAURI_API_EVENT from "@tauri-apps/api/event";
import type { ControllerKind } from "../bindings";

export type Result<T, E> = { status: "ok"; data: T } | { status: "error"; error: E };

async function call<T, E>(cmd: string, args?: Record<string, unknown>): Promise<Result<T, E>> {
  try {
    return { status: "ok", data: await TAURI_INVOKE<T>(cmd, args) };
  } catch (e) {
    if (e instanceof Error) throw e;
    return { status: "error", error: e as E };
  }
}

// ------------------------------------------------------------------------------------------------
// Shared types

export type Os = "windows" | "linux" | "macos";
export type Arch = "x86_64" | "aarch64";
export type Role = "user" | "admin" | "owner";

/** Generic error for commands without a more specific error type. Messages are English fallbacks. */
export type AppError =
  | { kind: "network"; detail: string }
  | { kind: "server"; code: string; message: string }
  | { kind: "unauthenticated" }
  | { kind: "not_found" }
  | { kind: "invalid_input"; field: string; message: string }
  | { kind: "io"; path: string | null; detail: string }
  | { kind: "internal"; detail: string };

export type Theme = "system" | "dark" | "light" | "high_contrast";

export type AppearanceSettings = {
  theme: Theme;
  /** Forces reduced motion regardless of the OS setting. */
  reduce_motion: boolean;
};

// ------------------------------------------------------------------------------------------------
// Servers and accounts (A2-T07)

export type Account = {
  user_id: string;
  username: string;
  display_name: string | null;
  role: Role;
};

export type ServerProfile = {
  id: string;
  url: string;
  name: string;
  /** `VG1-XXXX-…` root key fingerprint, pinned when the server was added. */
  fingerprint: string;
  active: boolean;
  account: Account | null;
  last_connected_at: string | null;
};

export type RegistrationMode = "open" | "allowlist" | "closed";

export type ServerPreview = {
  /** Opaque handle for `server_confirm`; the preview is kept in Rust, never re-sent from the UI. */
  preview_id: string;
  url: string;
  server_id: string;
  name: string;
  motd: string | null;
  fingerprint: string;
  registration_mode: RegistrationMode | null;
  /** Set when the preview came from a `vgames://server/add?…&fp=` link (already checked equal). */
  expected_fingerprint: string | null;
};

export type ServerError =
  | { kind: "invalid_url" }
  | { kind: "insecure_scheme" }
  | { kind: "unreachable"; detail: string }
  | { kind: "timeout" }
  | { kind: "tls"; detail: string }
  | { kind: "not_vgames" }
  | { kind: "launcher_too_old"; min_version: string; current_version: string }
  | { kind: "fingerprint_mismatch"; expected: string; actual: string }
  | { kind: "already_added"; server_id: string }
  | { kind: "preview_expired" };

export type AuthFlow = {
  flow_id: string;
  expires_at: string;
  /** False when the system browser could not be opened; the UI then offers the paste-code fallback. */
  browser_opened: boolean;
};

export type AuthError =
  | { kind: "registration_closed" }
  | { kind: "not_allowlisted" }
  | { kind: "user_disabled" }
  | { kind: "expired" }
  | { kind: "invalid_code" }
  | { kind: "cancelled" }
  | { kind: "browser_unavailable" }
  | { kind: "network"; detail: string }
  | { kind: "server"; code: string; message: string };

export type AuthOutcome =
  | { kind: "signed_in"; account: Account }
  | { kind: "failed"; error: AuthError };

// ------------------------------------------------------------------------------------------------
// Libraries (A2-T08)

export type Library = {
  id: string;
  path: string;
  label: string | null;
  is_default: boolean;
  /** False when the drive or folder is missing ("library offline"). */
  online: boolean;
  free_bytes: number | null;
  total_bytes: number | null;
  install_count: number;
};

export type FolderPick = {
  path: string;
  free_bytes: number;
  total_bytes: number;
};

export type LibraryError =
  | { kind: "not_writable" }
  | { kind: "system_directory" }
  | { kind: "nested_in_library"; library_path: string }
  | { kind: "contains_library"; library_path: string }
  | { kind: "already_added" }
  | { kind: "not_found" }
  | { kind: "io"; detail: string };

// ------------------------------------------------------------------------------------------------
// Events

export type NavAction =
  | "up"
  | "down"
  | "left"
  | "right"
  | "accept"
  | "back"
  | "menu"
  | "options"
  | "tab_prev"
  | "tab_next";

/**
 * A navigation intent from a game controller, already debounced and auto-repeated by the Rust
 * controllers module (D-pad and left stick → directions; south → accept; east → back; north → menu;
 * Start/Options → options; LB/RB → tab_prev/tab_next).
 */
export type UiNav = { action: NavAction; controller: ControllerKind; repeat: boolean };

/** The controller family that last produced input, for button glyphs. `null` when none is connected. */
export type ActiveControllerChanged = { controller: ControllerKind | null };

export type ConnectivityChanged = { server_id: string; online: boolean };

export type TrustProblem = {
  server_id: string;
  kind: "fingerprint_mismatch";
  server_name: string;
  pinned_fingerprint: string;
  presented_fingerprint: string;
};

/** A `vgames://server/add` link was opened; the UI starts onboarding with these values. */
export type ServerAddRequested = { url: string; fingerprint: string };

export type AuthFinished = { flow_id: string; outcome: AuthOutcome };

export type ServersChanged = Record<string, never>;
export type LibrariesChanged = Record<string, never>;

// ------------------------------------------------------------------------------------------------
// Commands

export const pendingCommands = {
  /** Redacted diagnostics text for bug reports (no tokens, ids or paths under the home directory). */
  async appDiagnostics(): Promise<string> {
    return await TAURI_INVOKE("app_diagnostics");
  },
  /** Opens an http(s) URL in the system browser. Rust rejects every other scheme. */
  async openExternalUrl(url: string): Promise<Result<null, AppError>> {
    return call("open_external_url", { url });
  },
  async appearanceGet(): Promise<AppearanceSettings> {
    return await TAURI_INVOKE("appearance_get");
  },
  async appearanceSet(settings: AppearanceSettings): Promise<Result<AppearanceSettings, AppError>> {
    return call("appearance_set", { settings });
  },

  async serversList(): Promise<ServerProfile[]> {
    return await TAURI_INVOKE("servers_list");
  },
  /** Fetches `/.well-known/vgames.json`. `expectedFingerprint` comes from a `vgames://server/add` link. */
  async serverPreview(
    url: string,
    expectedFingerprint: string | null,
  ): Promise<Result<ServerPreview, ServerError>> {
    return call("server_preview", { url, expectedFingerprint });
  },
  /** Pins the previewed root key and makes the server active. */
  async serverConfirm(previewId: string): Promise<Result<ServerProfile, ServerError>> {
    return call("server_confirm", { previewId });
  },
  async serverSwitch(serverId: string): Promise<Result<ServerProfile, AppError>> {
    return call("server_switch", { serverId });
  },
  async serverRemove(serverId: string): Promise<Result<null, AppError>> {
    return call("server_remove", { serverId });
  },

  /** Starts Discord sign-in for a server and opens the system browser. */
  async authStart(serverId: string): Promise<Result<AuthFlow, AuthError>> {
    return call("auth_start", { serverId });
  },
  async authOpenBrowser(flowId: string): Promise<Result<null, AuthError>> {
    return call("auth_open_browser", { flowId });
  },
  /** The paste-code fallback when the `vgames://` callback cannot reach the launcher. */
  async authSubmitCode(flowId: string, code: string): Promise<Result<Account, AuthError>> {
    return call("auth_submit_code", { flowId, code });
  },
  async authCancel(flowId: string): Promise<null> {
    return await TAURI_INVOKE("auth_cancel", { flowId });
  },

  /** Revokes this device's session on the server and deletes its tokens from the keychain. */
  async authSignOut(serverId: string): Promise<Result<null, AppError>> {
    return call("auth_sign_out", { serverId });
  },

  async librariesList(): Promise<Result<Library[], AppError>> {
    return call("libraries_list");
  },
  /** Opens the native folder picker (Rust side). `null` when the user cancels. */
  async libraryPickFolder(): Promise<Result<FolderPick | null, AppError>> {
    return call("library_pick_folder");
  },
  async libraryAdd(path: string, makeDefault: boolean): Promise<Result<Library, LibraryError>> {
    return call("library_add", { path, makeDefault });
  },
};

// ------------------------------------------------------------------------------------------------
// Events (same helper shape as tauri-specta's `__makeEvents__`)

type EventApi<T> = {
  listen: (cb: TAURI_API_EVENT.EventCallback<T>) => ReturnType<typeof TAURI_API_EVENT.listen<T>>;
  once: (cb: TAURI_API_EVENT.EventCallback<T>) => ReturnType<typeof TAURI_API_EVENT.once<T>>;
  emit: (payload: T) => ReturnType<typeof TAURI_API_EVENT.emit>;
};

function makeEvents<T extends Record<string, unknown>>(mappings: Record<keyof T, string>) {
  const out = {} as { [K in keyof T]: EventApi<T[K]> };
  for (const key of Object.keys(mappings) as (keyof T)[]) {
    const name = mappings[key];
    out[key] = {
      listen: (cb) => TAURI_API_EVENT.listen(name, cb),
      once: (cb) => TAURI_API_EVENT.once(name, cb),
      emit: (payload) => TAURI_API_EVENT.emit(name, payload),
    };
  }
  return out;
}

export const pendingEvents = makeEvents<{
  uiNav: UiNav;
  activeControllerChanged: ActiveControllerChanged;
  connectivityChanged: ConnectivityChanged;
  trustProblem: TrustProblem;
  serverAddRequested: ServerAddRequested;
  authFinished: AuthFinished;
  serversChanged: ServersChanged;
  librariesChanged: LibrariesChanged;
}>({
  uiNav: "ui-nav",
  activeControllerChanged: "active-controller-changed",
  connectivityChanged: "connectivity-changed",
  trustProblem: "trust-problem",
  serverAddRequested: "server-add-requested",
  authFinished: "auth-finished",
  serversChanged: "servers-changed",
  librariesChanged: "libraries-changed",
});
