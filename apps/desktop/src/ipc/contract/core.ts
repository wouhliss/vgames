// Commands and events the UI needs that are NOT in the generated `src/bindings.ts` yet: app,
// servers, accounts and libraries (Agent 2, A2-T01/T07/T08).
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
import type { AppError, ControllerKind } from "../../bindings";
import { call, get, makeEvents, type Result } from "./runtime";

export type { Result } from "./runtime";

// ------------------------------------------------------------------------------------------------
// Shared types

export type Os = "windows" | "linux" | "macos";
export type Arch = "x86_64" | "aarch64";
/** A package build's target (OpenAPI `Platform`). */
export type Platform =
  | "windows-x86_64"
  | "windows-aarch64"
  | "linux-x86_64"
  | "linux-aarch64"
  | "macos-aarch64"
  | "macos-x86_64";

export type Theme = "system" | "dark" | "light" | "high_contrast";

export type AppearanceSettings = {
  theme: Theme;
  /** Forces reduced motion regardless of the OS setting. */
  reduce_motion: boolean;
};

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

export type LibrariesChanged = Record<string, never>;

// ------------------------------------------------------------------------------------------------
// Commands

export const coreCommands = {
  /** Redacted diagnostics text for bug reports (no tokens, ids or paths under the home directory). */
  async appDiagnostics(): Promise<string> {
    return await get("app_diagnostics");
  },
  /** Opens an http(s) URL in the system browser. Rust rejects every other scheme. */
  async openExternalUrl(url: string): Promise<Result<null, AppError>> {
    return call("open_external_url", { url });
  },
  async appearanceGet(): Promise<AppearanceSettings> {
    return await get("appearance_get");
  },
  async appearanceSet(settings: AppearanceSettings): Promise<Result<AppearanceSettings, AppError>> {
    return call("appearance_set", { settings });
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
// Events

export const coreEvents = makeEvents<{
  uiNav: UiNav;
  activeControllerChanged: ActiveControllerChanged;
  librariesChanged: LibrariesChanged;
}>({
  uiNav: "ui-nav",
  activeControllerChanged: "active-controller-changed",
  librariesChanged: "libraries-changed",
});
