// Installed packages, favorites, collections, launching and per-install actions (Agent 2,
// A2-T06/T08/T09/T10). Requested shapes; see core.ts for the conventions.
import type { AppError, PackageRef } from "../../bindings";
import type { Platform } from "./core";
import { call, makeEvents, type Result } from "./runtime";

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

/** A user category. Collections are local and span servers; items carry their server. */
export type Collection = {
  id: string;
  /** 1–100 characters. */
  name: string;
  position: number;
};

export type CollectionError =
  | { kind: "invalid_name" }
  | { kind: "name_taken" }
  | { kind: "not_found" };

export type LaunchError =
  | { kind: "not_installed" }
  | { kind: "incomplete" }
  | { kind: "busy"; state: InstallState }
  | { kind: "library_offline"; library_path: string }
  | { kind: "already_running" }
  | { kind: "target_not_found" }
  /** A file no longer matches the signed manifest (02 §11). "Verify" repairs it. */
  | { kind: "integrity"; path: string }
  /** The publisher key was revoked; "Verify" fetches a re-signed manifest. */
  | { kind: "key_revoked" }
  /** Proton/Wine, Vulkan, Rosetta or another compat prerequisite is missing. */
  | { kind: "compat_unavailable"; detail: string }
  /** One launch per 3 s (01-security §7). */
  | { kind: "rate_limited" }
  /** A cloud save conflict must be resolved first (the `save-conflict` event carries it). */
  | { kind: "save_conflict"; conflict_id: string }
  | { kind: "io"; detail: string };

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

export type InstallsChanged = Record<string, never>;
export type CollectionsChanged = Record<string, never>;

export const libraryCommands = {
  /** Installed (and incomplete) packages of the active server. */
  async installsList(): Promise<Result<InstalledPackage[], AppError>> {
    return call("installs_list");
  },
  async collectionsList(): Promise<Result<Collection[], AppError>> {
    return call("collections_list");
  },
  async collectionCreate(name: string): Promise<Result<Collection, CollectionError>> {
    return call("collection_create", { name });
  },
  async collectionRename(
    collectionId: string,
    name: string,
  ): Promise<Result<Collection, CollectionError>> {
    return call("collection_rename", { collectionId, name });
  },
  async collectionDelete(collectionId: string): Promise<Result<null, CollectionError>> {
    return call("collection_delete", { collectionId });
  },
  /** The full new order; ids not listed keep their relative order after the listed ones. */
  async collectionsReorder(collectionIds: string[]): Promise<Result<null, CollectionError>> {
    return call("collections_reorder", { collectionIds });
  },
  async collectionAddPackage(
    collectionId: string,
    pkg: PackageRef,
  ): Promise<Result<null, CollectionError>> {
    return call("collection_add_package", { collectionId, package: pkg });
  },
  async collectionRemovePackage(
    collectionId: string,
    pkg: PackageRef,
  ): Promise<Result<null, CollectionError>> {
    return call("collection_remove_package", { collectionId, package: pkg });
  },
  async favoriteSet(pkg: PackageRef, favorite: boolean): Promise<Result<null, AppError>> {
    return call("favorite_set", { package: pkg, favorite });
  },

  /** Pre-launch checks, cloud-save pull, then spawn (A2-T09). `targetId` null = default target. */
  async gameLaunch(pkg: PackageRef, targetId: string | null): Promise<Result<null, LaunchError>> {
    return call("game_launch", { package: pkg, targetId });
  },
  /** Terminates the game's process tree (the UI confirms first). */
  async gameStop(pkg: PackageRef): Promise<Result<null, AppError>> {
    return call("game_stop", { package: pkg });
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

export const libraryEvents = makeEvents<{
  installsChanged: InstallsChanged;
  collectionsChanged: CollectionsChanged;
}>({
  installsChanged: "installs-changed",
  collectionsChanged: "collections-changed",
});
