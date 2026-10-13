// PLAY-06 pending native interfaces; see 06-cloud-saves-notes.md. Native implementations
// replace these entries with generated bindings as each command/event lands.
import type { AppError, CloudSaveState, PackageRef } from "../../bindings";
import { call, makeEvents, type Result } from "./runtime";

export type SaveSource =
  | { kind: "cloud"; snapshot_id: string }
  | { kind: "backup"; backup_id: string };
export type SaveStatus = {
  package: PackageRef;
  state: CloudSaveState;
  last_synced_at: string | null;
  conflict_id: string | null;
};
export type SaveSummary = {
  source: SaveSource;
  created_at: string;
  device_name: string;
  file_count: number;
  total_bytes: number;
};
export type SaveHistory = {
  package: PackageRef;
  entries: SaveSummary[];
  head_snapshot_id: string | null;
};
export type SaveConflict = {
  conflict_id: string;
  package: PackageRef;
  cloud: SaveSummary;
  device: SaveSummary;
  created_at: string;
};
export type ConflictChoice = "keep_cloud" | "keep_device" | "keep_both" | "cancel";
export const savesCommands = {
  savesStatus(pkg: PackageRef): Promise<Result<SaveStatus, AppError>> {
    return call("saves_status", { pkg });
  },
  savesHistory(pkg: PackageRef): Promise<Result<SaveHistory, AppError>> {
    return call("saves_history", { pkg });
  },
  savesRestore(pkg: PackageRef, source: SaveSource): Promise<Result<SaveStatus, AppError>> {
    return call("saves_restore", { pkg, source });
  },
  saveConflictGet(conflictId: string): Promise<Result<SaveConflict, AppError>> {
    return call("save_conflict_get", { conflictId });
  },
  saveConflictResolve(
    conflictId: string,
    choice: ConflictChoice,
  ): Promise<Result<SaveStatus, AppError>> {
    return call("save_conflict_resolve", { conflictId, choice });
  },
};
export const savesEvents = makeEvents<{
  saveSyncChanged: SaveStatus;
  saveConflict: SaveConflict;
}>({ saveSyncChanged: "save-sync-changed", saveConflict: "save-conflict" });
