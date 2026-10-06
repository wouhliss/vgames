// Installed packages and per-install actions (INS-04). Collections and favorites are generated
// now. Requested shapes; see core.ts for the conventions.
import type { AppError, PackageRef } from "../../bindings";
import type { Platform } from "./core";
import { call, type Result } from "./runtime";

/**
 * Where an install is. `incomplete` = `install.json` is not `installed` and no download job exists
 * (02-package-format §10: "resume or remove"). `installing` covers queued and active downloads.
 */
export type InstallState =
  | "installed"
  | "incomplete"
  | "installing"
  | "updating"
  | "repairing"
  | "moving"
  | "uninstalling";

export type LaunchTarget = {
  id: string;
  label: string;
  /** The manifest's `launch.default`. */
  is_default: boolean;
};

/** How the package runs on this machine (09-compatibility §1). */
export type CompatLayer = "native" | "proton" | "wine";

/** Cloud save state of an install (06-cloud-saves). `unsupported`: the manifest declares no saves. */
export type CloudSaveState = "unsupported" | "synced" | "syncing" | "pending" | "conflict";

export type AvailableUpdate = {
  version_label: string;
  sequence: number;
  /** Bytes to download (changed files only). */
  download_bytes: number;
  /** The installed version was withdrawn: this release is offered even if its sequence is lower. */
  installed_yanked: boolean;
};

export type InstalledPackage = {
  package: PackageRef;
  slug: string;
  title: string;
  /** `vgimg:` URL of the cached cover, served by the Rust core. */
  cover_url: string | null;
  library_id: string;
  /** Absolute install directory, for display only. */
  install_path: string;
  platform: Platform;
  version_label: string;
  sequence: number;
  size_bytes: number;
  installed_at: string | null;
  last_played_at: string | null;
  playtime_seconds: number;
  state: InstallState;
  update: AvailableUpdate | null;
  favorite: boolean;
  collection_ids: string[];
  running: boolean;
  /** Empty for incomplete installs (the manifest is not verified yet). */
  targets: LaunchTarget[];
  compat: CompatLayer;
  cloud_saves: CloudSaveState;
};

export type InstallActionError =
  | { kind: "not_found" }
  | { kind: "busy"; state: InstallState }
  | { kind: "running" }
  | { kind: "library_offline"; library_path: string }
  | { kind: "offline" }
  | { kind: "insufficient_space"; required_bytes: number; available_bytes: number }
  | { kind: "same_library" }
  | { kind: "io"; detail: string };

/** What uninstalling removes, and what the user must decide about (02-package-format §9). */
export type UninstallPlan = {
  size_bytes: number;
  /** Files not listed in the manifest (mods, configs, local saves): at most 50, relative paths. */
  leftovers: { path: string; size_bytes: number }[];
  leftover_count: number;
  leftover_bytes: number;
  /** A Proton/Wine prefix exists; it may hold local saves (09-compatibility §2). */
  has_prefix: boolean;
};

export const libraryCommands = {
  /** Installed (and incomplete) packages of the active server. */
  async installsList(): Promise<Result<InstalledPackage[], AppError>> {
    return call("installs_list");
  },

  /** Queues the available update; it shows up in Downloads. */
  async installUpdate(pkg: PackageRef): Promise<Result<null, InstallActionError>> {
    return call("install_update", { package: pkg });
  },
  /** Re-hashes every file and repairs mismatches (02 §9). */
  async installVerify(pkg: PackageRef): Promise<Result<null, InstallActionError>> {
    return call("install_verify", { package: pkg });
  },
  /** Queues an incomplete install again from its journal. */
  async installResume(pkg: PackageRef): Promise<Result<null, InstallActionError>> {
    return call("install_resume", { package: pkg });
  },
  async installMove(pkg: PackageRef, libraryId: string): Promise<Result<null, InstallActionError>> {
    return call("install_move", { package: pkg, libraryId });
  },
  async installUninstallPlan(pkg: PackageRef): Promise<Result<UninstallPlan, InstallActionError>> {
    return call("install_uninstall_plan", { package: pkg });
  },
  async installUninstall(
    pkg: PackageRef,
    removeLeftovers: boolean,
    removePrefix: boolean,
  ): Promise<Result<null, InstallActionError>> {
    return call("install_uninstall", { package: pkg, removeLeftovers, removePrefix });
  },
  /** Opens the install directory in the system file manager. */
  async installOpenFolder(pkg: PackageRef): Promise<Result<null, AppError>> {
    return call("install_open_folder", { package: pkg });
  },
};
