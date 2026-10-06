// Installed packages and per-install actions (INS-04). Collections and favorites are generated
// now. Requested shapes; see core.ts for the conventions.
import type { AppError, InstallState, PackageRef } from "../../bindings";
import { call, type Result } from "./runtime";

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
