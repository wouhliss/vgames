// Starting an install (INS-03) and installing Rosetta 2 (GAME). The catalog itself (`catalog_list`,
// `catalog_genres`, `package_details`, `install_plan` and their types) is generated now (INS-02).
// Requested shapes; see core.ts for the conventions.
import type { InstallPlanError } from "../../bindings";
import type { AppError } from "./core";
import { call, type Result } from "./runtime";

export type InstallStartError =
  | InstallPlanError
  | { kind: "insufficient_space"; required_bytes: number; available_bytes: number }
  | { kind: "library_offline"; library_path: string }
  /** The server's trust bundle expired: installed games still launch, new installs wait (01-security §3.2). */
  | { kind: "trust_expired" }
  | { kind: "io"; detail: string };

export const catalogCommands = {
  /** Queues the install into `libraryId`; progress follows through `install-progress`. */
  async installStart(
    packageId: string,
    libraryId: string,
  ): Promise<Result<null, InstallStartError>> {
    return call("install_start", { packageId, libraryId });
  },
  /** macOS: `softwareupdate --install-rosetta` (the UI asks for confirmation first). */
  async rosettaInstall(): Promise<Result<null, AppError>> {
    return call("rosetta_install");
  },
};
