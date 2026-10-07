// Installing Rosetta 2 (GAME). The catalog and `install_start` are generated now (INS-02, INS-03).
// Requested shapes; see core.ts for the conventions.
import type { AppError } from "./core";
import { call, type Result } from "./runtime";

export const catalogCommands = {
  /** macOS: `softwareupdate --install-rosetta` (the UI asks for confirmation first). */
  async rosettaInstall(): Promise<Result<null, AppError>> {
    return call("rosetta_install");
  },
};
