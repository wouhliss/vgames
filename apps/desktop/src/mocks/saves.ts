// Deterministic save state for conflict/history UI tests. This mock never touches save files.
import type {
  AppError,
  PackageRef,
  SaveConflict,
  SaveHistory,
  SaveSource,
  SaveStatus,
} from "../ipc";
import { events } from "../ipc";
import { fail, type Handler, mockId } from "./runtime";

export interface SaveRecord {
  status: SaveStatus;
  history: SaveHistory;
  conflict: SaveConflict | null;
  operationError: AppError | null;
  running: boolean;
}
export interface SavesState {
  saveRecords: Record<string, SaveRecord>;
}
export function defaultSavesState(): SavesState {
  return { saveRecords: {} };
}
export function saveKey(pkg: PackageRef): string {
  return `${pkg.server_id}/${pkg.package_id}`;
}
export function conflictFixture(pkg: PackageRef): SaveRecord {
  const cloud = {
    source: { kind: "cloud" as const, snapshot_id: "01920001-0000-7000-8000-000000000001" },
    created_at: "2026-10-05T09:00:00Z",
    device_name: "Other computer",
    file_count: 3,
    total_bytes: 1024,
  };
  const device = {
    source: { kind: "backup" as const, backup_id: "01920001-0000-7000-8000-000000000002" },
    created_at: "2026-10-05T10:00:00Z",
    device_name: "This computer",
    file_count: 4,
    total_bytes: 2048,
  };
  const conflict_id = "01920001-0000-7000-8000-000000000003";
  return {
    status: { package: pkg, state: "conflict", last_synced_at: null, conflict_id },
    history: { package: pkg, entries: [cloud, device], head_snapshot_id: cloud.source.snapshot_id },
    conflict: { conflict_id, package: pkg, cloud, device, created_at: device.created_at },
    operationError: null,
    running: false,
  };
}
function packageArg(args: Record<string, unknown>): PackageRef {
  const pkg = args.pkg as PackageRef | undefined;
  if (!pkg?.server_id || !pkg.package_id)
    fail({ kind: "invalid_input", field: "pkg", message: "Choose a game." } satisfies AppError);
  return pkg;
}
function sameSource(a: SaveSource, b: SaveSource): boolean {
  return a.kind === "cloud" && b.kind === "cloud"
    ? a.snapshot_id === b.snapshot_id
    : a.kind === "backup" && b.kind === "backup" && a.backup_id === b.backup_id;
}
export function savesHandlers(state: SavesState): Record<string, Handler> {
  let seq = 100;
  const nextId = () => {
    const used = new Set(
      Object.values(state.saveRecords).flatMap((entry) =>
        entry.history.entries.map((item) =>
          item.source.kind === "cloud" ? item.source.snapshot_id : item.source.backup_id,
        ),
      ),
    );
    let id = mockId("01920001", ++seq);
    while (used.has(id)) id = mockId("01920001", ++seq);
    return id;
  };
  const record = (pkg: PackageRef) => state.saveRecords[saveKey(pkg)];
  const conflict = (id: unknown) => {
    const entry = Object.values(state.saveRecords).find(
      (entry) => entry.conflict?.conflict_id === id,
    );
    if (!entry?.conflict) fail({ kind: "not_found" } satisfies AppError);
    return entry;
  };
  const writable = (entry: SaveRecord) => {
    if (entry.running)
      fail({
        kind: "server",
        code: "save_game_running",
        message: "Close this game before changing its saves.",
      } satisfies AppError);
    if (entry.operationError) fail(entry.operationError);
  };
  const synced = (entry: SaveRecord) => {
    entry.status = {
      ...entry.status,
      state: "synced",
      last_synced_at: "2026-10-05T10:01:00Z",
      conflict_id: null,
    };
    entry.conflict = null;
    void events.saveSyncChanged.emit(entry.status);
    return entry.status;
  };
  return {
    saves_status: (args) => {
      const pkg = packageArg(args);
      return (
        record(pkg)?.status ??
        ({
          package: pkg,
          state: "unsupported",
          last_synced_at: null,
          conflict_id: null,
        } satisfies SaveStatus)
      );
    },
    saves_history: (args) => {
      const pkg = packageArg(args);
      return (
        record(pkg)?.history ??
        ({ package: pkg, entries: [], head_snapshot_id: null } satisfies SaveHistory)
      );
    },
    saves_restore: (args) => {
      const entry = record(packageArg(args));
      if (!entry) fail({ kind: "not_found" } satisfies AppError);
      const source = args.source as SaveSource;
      if (!source || !entry.history.entries.some((item) => sameSource(item.source, source)))
        fail({ kind: "not_found" } satisfies AppError);
      writable(entry);
      const selected = entry.history.entries.find((item) => sameSource(item.source, source));
      if (!selected) fail({ kind: "not_found" } satisfies AppError);
      const snapshot_id = nextId();
      entry.history.entries.unshift({ ...selected, source: { kind: "cloud", snapshot_id } });
      entry.history.head_snapshot_id = snapshot_id;
      return synced(entry);
    },
    save_conflict_get: (args) => conflict(args.conflictId).conflict,
    save_conflict_resolve: (args) => {
      const entry = conflict(args.conflictId);
      if (args.choice === "cancel") return entry.status;
      if (!["keep_cloud", "keep_device", "keep_both"].includes(String(args.choice)))
        fail({
          kind: "invalid_input",
          field: "choice",
          message: "Choose how to keep your saves.",
        } satisfies AppError);
      writable(entry);
      const current = entry.conflict;
      if (!current) fail({ kind: "not_found" } satisfies AppError);
      if (args.choice !== "keep_cloud") {
        if (args.choice === "keep_both")
          entry.history.entries.unshift({
            ...current.cloud,
            source: { kind: "backup", backup_id: nextId() },
          });
        const snapshot_id = nextId();
        entry.history.entries.unshift({
          ...current.device,
          source: { kind: "cloud", snapshot_id },
        });
        entry.history.head_snapshot_id = snapshot_id;
      }
      return synced(entry);
    },
  };
}
