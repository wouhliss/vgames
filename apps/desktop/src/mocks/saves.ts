// Cloud-save fixtures for PLAY-06: per-package status and durable conflicts. A conflict also makes
// `game_launch` refuse with `save_conflict` until it is resolved, as the Rust launch hook does.
import type {
  AppError,
  ConflictChoice,
  LaunchError,
  PackageRef,
  SaveConflict,
  SaveHistory,
  SaveSource,
  SaveStatus,
} from "../ipc";
import { fail, type Handler } from "./runtime";

export interface SavesState {
  saveConflicts: Record<string, SaveConflict>;
  /** What the next `save_conflict_resolve` returns instead of resolving. */
  saveResolveError: AppError | null;
  /** The next resolution finds the cloud changed again: the conflict stays with a new cloud side. */
  saveConflictRenews: boolean;
  /** History per package id; packages without one have an empty history. */
  saveHistories: Record<string, SaveHistory>;
  /** What the next `saves_restore` returns instead of restoring. */
  saveRestoreError: AppError | null;
}

export function defaultSavesState(): SavesState {
  return {
    saveConflicts: {},
    saveResolveError: null,
    saveConflictRenews: false,
    saveHistories: {},
    saveRestoreError: null,
  };
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

/** Two cloud snapshots (the newest is the head) and one backup from this device. */
export function makeSaveHistory(pkg: PackageRef): SaveHistory {
  return {
    package: pkg,
    head_snapshot_id: "5a5e0000-0000-7000-8000-000000000003",
    entries: [
      {
        source: { kind: "cloud", snapshot_id: "5a5e0000-0000-7000-8000-000000000003" },
        created_at: "2026-10-24T20:10:00Z",
        device_name: "Living room PC",
        file_count: 3,
        total_bytes: 2_400_000,
      },
      {
        source: { kind: "backup", backup_id: "bacc0000-0000-7000-8000-000000000002" },
        created_at: "2026-10-24T18:00:00Z",
        device_name: null,
        file_count: 2,
        total_bytes: 1_900_000,
      },
      {
        source: { kind: "cloud", snapshot_id: "5a5e0000-0000-7000-8000-000000000002" },
        created_at: "2026-10-22T09:30:00Z",
        device_name: "Laptop",
        file_count: 2,
        total_bytes: 1_800_000,
      },
    ],
  };
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
  let restores = Object.values(state.saveHistories).flatMap((h) => h.entries).length;
  return {
    saves_status: (args) => status(args.pkg as PackageRef),
    saves_history: (args) => {
      const pkg = args.pkg as PackageRef;
      return (
        state.saveHistories[pkg.package_id] ?? {
          package: pkg,
          entries: [],
          head_snapshot_id: null,
        }
      );
    },
    saves_restore: (args) => {
      const pkg = args.pkg as PackageRef;
      if (state.saveRestoreError) {
        const error = state.saveRestoreError;
        state.saveRestoreError = null;
        fail(error);
      }
      const source = args.source as SaveSource;
      const history = state.saveHistories[pkg.package_id];
      const known = history?.entries.some(
        (entry) => JSON.stringify(entry.source) === JSON.stringify(source),
      );
      if (!history || !known) fail({ kind: "not_found" } satisfies AppError);
      // Like the Rust service: the current files are backed up before anything is written.
      restores += 1;
      state.saveHistories[pkg.package_id] = {
        ...history,
        entries: [
          {
            source: {
              kind: "backup",
              backup_id: `bacc0000-0000-7000-8000-${String(restores).padStart(12, "0")}`,
            },
            created_at: new Date().toISOString(),
            device_name: null,
            file_count: 3,
            total_bytes: 2_400_000,
          },
          ...history.entries,
        ],
      };
      return {
        package: pkg,
        state: "pending",
        last_synced_at: "2026-10-24T08:00:00Z",
        conflict_id: null,
      } satisfies SaveStatus;
    },
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
