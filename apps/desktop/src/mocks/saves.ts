// Cloud saves: conflicts, history and restore (06-cloud-saves §3). A conflict is created on the fly
// for a package whose `cloud_saves` is `conflict` (the id is `conflict-<package id>`), so the library
// fixtures and the tests share one scenario; `state.saves` holds what a test wants to control.
import type {
  InstalledPackage,
  PackageRef,
  SaveBackup,
  SaveChoice,
  SaveConflict,
  SaveConflictError,
  SaveHistory,
  SaveHistoryError,
  SaveRestoreError,
  SaveSnapshot,
} from "../ipc";
import { events } from "../ipc";
import { fail, type Handler } from "./runtime";

export interface SavesState {
  saves: {
    /** Conflicts by id; one is made up for a `conflict` install that has none. */
    conflicts: Record<string, SaveConflict>;
    /** History by package id; a default one is made up. */
    history: Record<string, SaveHistory>;
    /** Forced results by command (`saves_resolve`, `saves_history`, `saves_restore`). */
    errors: Record<string, SaveConflictError | SaveHistoryError | SaveRestoreError>;
    /** The cloud is out of reach: history can't list snapshots, restores and resolves fail. */
    offline: boolean;
    /** Conflict ids resolved so far, with the choice. */
    resolved: { conflict_id: string; choice: SaveChoice }[];
  };
}

export function defaultSavesState(): SavesState {
  return { saves: { conflicts: {}, history: {}, errors: {}, offline: false, resolved: [] } };
}

const MIB = 1024 ** 2;

export const conflictIdFor = (ref: PackageRef) => `conflict-${ref.package_id}`;

export function makeConflict(pkg: InstalledPackage): SaveConflict {
  return {
    conflict_id: conflictIdFor(pkg.package),
    package: pkg.package,
    title: pkg.title,
    local: {
      changed_at: "2026-09-29T21:14:00Z",
      device_name: "Sam's desktop",
      file_count: 4,
      size_bytes: 3 * MIB,
    },
    cloud: {
      changed_at: "2026-09-29T18:02:00Z",
      device_name: "Steam Deck",
      file_count: 5,
      size_bytes: 4 * MIB,
    },
  };
}

function defaultHistory(): SaveHistory {
  const snapshots: SaveSnapshot[] = [
    {
      snapshot_id: "snap-3",
      created_at: "2026-09-29T18:02:00Z",
      device_name: "Steam Deck",
      file_count: 5,
      size_bytes: 4 * MIB,
      current: true,
    },
    {
      snapshot_id: "snap-2",
      created_at: "2026-09-27T20:30:00Z",
      device_name: "Sam's desktop",
      file_count: 4,
      size_bytes: 3 * MIB,
      current: false,
    },
    {
      snapshot_id: "snap-1",
      created_at: "2026-09-20T09:00:00Z",
      device_name: "Sam's desktop",
      file_count: 2,
      size_bytes: MIB,
      current: false,
    },
  ];
  const backups: SaveBackup[] = [
    {
      backup_id: "backup-2",
      created_at: "2026-09-29T18:05:00Z",
      file_count: 4,
      size_bytes: 3 * MIB,
      reason: "restore",
    },
    {
      backup_id: "backup-1",
      created_at: "2026-09-27T20:31:00Z",
      file_count: 2,
      size_bytes: MIB,
      reason: "conflict",
    },
  ];
  return { snapshots, backups };
}

export function savesHandlers(
  state: SavesState & { installs: InstalledPackage[] },
): Record<string, Handler> {
  const forced = (cmd: string) => {
    const error = state.saves.errors[cmd];
    if (error) fail(error);
  };
  const installOf = (ref: PackageRef) =>
    state.installs.find((i) => i.package.package_id === ref.package_id);
  const setSaveState = (ref: PackageRef, cloud_saves: InstalledPackage["cloud_saves"]) => {
    state.installs = state.installs.map((i) =>
      i.package.package_id === ref.package_id ? { ...i, cloud_saves } : i,
    );
    void events.installsChanged.emit({});
  };
  const conflict = (id: string): SaveConflict => {
    const known = state.saves.conflicts[id];
    if (known) return known;
    const pkg = state.installs.find(
      (i) => conflictIdFor(i.package) === id && i.cloud_saves === "conflict",
    );
    if (!pkg) fail({ kind: "not_found" } satisfies SaveConflictError);
    return makeConflict(pkg);
  };

  return {
    saves_conflict: (args) => conflict(String(args.conflictId)),
    saves_resolve: (args) => {
      forced("saves_resolve");
      const c = conflict(String(args.conflictId));
      if (state.saves.offline) fail({ kind: "offline" } satisfies SaveConflictError);
      const pkg = installOf(c.package);
      if (pkg?.running) fail({ kind: "running" } satisfies SaveConflictError);
      state.saves.resolved.push({ conflict_id: c.conflict_id, choice: args.choice as SaveChoice });
      delete state.saves.conflicts[c.conflict_id];
      setSaveState(c.package, "synced");
      void events.savesChanged.emit({ package: c.package });
      return { launched: false };
    },
    saves_history: (args) => {
      forced("saves_history");
      const ref = args.package as PackageRef;
      if (!installOf(ref)) fail({ kind: "not_found" } satisfies SaveHistoryError);
      const history = state.saves.history[ref.package_id] ?? defaultHistory();
      if (state.saves.offline) return { ...history, snapshots: [] };
      return history;
    },
    saves_restore: (args) => {
      forced("saves_restore");
      const ref = args.package as PackageRef;
      const pkg = installOf(ref);
      if (!pkg) fail({ kind: "not_found" } satisfies SaveRestoreError);
      if (pkg.running) fail({ kind: "running" } satisfies SaveRestoreError);
      const source = args.source as { kind: "snapshot" | "backup" };
      if (source.kind === "snapshot" && state.saves.offline)
        fail({ kind: "offline" } satisfies SaveRestoreError);
      void events.savesChanged.emit({ package: ref });
      return null;
    },
  };
}
