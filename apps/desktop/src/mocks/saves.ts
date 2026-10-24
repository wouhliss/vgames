// Cloud-save fixtures for PLAY-06: per-package status and durable conflicts. A conflict also makes
// `game_launch` refuse with `save_conflict` until it is resolved, as the Rust launch hook does.
import type {
  AppError,
  ConflictChoice,
  LaunchError,
  PackageRef,
  SaveConflict,
  SaveStatus,
} from "../ipc";
import { fail, type Handler } from "./runtime";

export interface SavesState {
  saveConflicts: Record<string, SaveConflict>;
  /** What the next `save_conflict_resolve` returns instead of resolving. */
  saveResolveError: AppError | null;
  /** The next resolution finds the cloud changed again: the conflict stays with a new cloud side. */
  saveConflictRenews: boolean;
}

export function defaultSavesState(): SavesState {
  return { saveConflicts: {}, saveResolveError: null, saveConflictRenews: false };
}

/** A conflict for `pkg` whose sides differ the way the dialog has to show. */
export function makeSaveConflict(pkg: PackageRef, id = "c0ffee00-0000-7000-8000-000000000001") {
  return {
    conflict_id: id,
    package: pkg,
    cloud: {
      source: { kind: "cloud", snapshot_id: "5a5e0000-0000-7000-8000-000000000001" },
      created_at: "2026-10-23T21:14:00Z",
      device_name: "Living room PC",
      file_count: 3,
      total_bytes: 2_400_000,
    },
    device: {
      source: { kind: "backup", backup_id: "bacc0000-0000-7000-8000-000000000001" },
      created_at: "2026-10-24T09:02:00Z",
      device_name: null,
      file_count: 4,
      total_bytes: 2_650_000,
    },
    created_at: "2026-10-24T09:02:00Z",
  } satisfies SaveConflict;
}

const same = (a: PackageRef, b: unknown) =>
  typeof b === "object" &&
  b !== null &&
  (b as PackageRef).server_id === a.server_id &&
  (b as PackageRef).package_id === a.package_id;

export function savesHandlers(
  state: SavesState & { launchErrors: Record<string, LaunchError> },
): Record<string, Handler> {
  const status = (pkg: PackageRef): SaveStatus => {
    const open = Object.values(state.saveConflicts).find((c) => same(c.package, pkg));
    return open
      ? { package: pkg, state: "conflict", last_synced_at: null, conflict_id: open.conflict_id }
      : {
          package: pkg,
          state: "synced",
          last_synced_at: "2026-10-24T08:00:00Z",
          conflict_id: null,
        };
  };
  return {
    saves_status: (args) => status(args.pkg as PackageRef),
    save_conflict_get: (args) => {
      const conflict = state.saveConflicts[String(args.conflictId)];
      if (!conflict) fail({ kind: "not_found" } satisfies AppError);
      return conflict;
    },
    save_conflict_resolve: (args) => {
      const conflict = state.saveConflicts[String(args.conflictId)];
      if (!conflict) fail({ kind: "not_found" } satisfies AppError);
      const choice = args.choice as ConflictChoice;
      if (choice === "cancel") return status(conflict.package);
      if (state.saveResolveError) {
        const error = state.saveResolveError;
        state.saveResolveError = null;
        fail(error);
      }
      if (state.saveConflictRenews) {
        state.saveConflictRenews = false;
        state.saveConflicts[conflict.conflict_id] = {
          ...conflict,
          cloud: { ...conflict.cloud, created_at: "2026-10-24T10:30:00Z", file_count: 5 },
        };
        return status(conflict.package);
      }
      delete state.saveConflicts[conflict.conflict_id];
      const forced = state.launchErrors[conflict.package.package_id];
      if (forced?.kind === "save_conflict") delete state.launchErrors[conflict.package.package_id];
      return status(conflict.package);
    },
  };
}
