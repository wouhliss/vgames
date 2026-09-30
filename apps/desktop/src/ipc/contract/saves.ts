// Cloud saves (06-cloud-saves §3): the conflict dialog, the sync notices and Settings → Cloud saves.
// Requested shapes; see core.ts for the conventions. The Rust core runs the sync algorithm (scan,
// decide, restore, push) and only asks the UI when a person has to choose: the UI never decides
// anything by itself, and a conflict is never resolved without an explicit choice.
import type { PackageRef } from "../../bindings";
import { call, makeEvents, type Result } from "./runtime";

/** One side of a conflict: the saves on this device, or the cloud head. */
export type SaveSide = {
  /** When the newest file of this side changed (local) or when the snapshot was pushed (cloud). */
  changed_at: string;
  /** The device that wrote it (this device's name for the local side). */
  device_name: string;
  file_count: number;
  size_bytes: number;
};

/** Both sides changed since the last sync. The game has not started (or has just exited). */
export type SaveConflict = {
  conflict_id: string;
  package: PackageRef;
  title: string;
  local: SaveSide;
  cloud: SaveSide;
};

/**
 * - `keep_cloud`: local files go to a backup, then the cloud head is restored.
 * - `keep_device`: this device's files are pushed on top of the cloud head.
 * - `keep_both`: this device's files are pushed as the new head; the old head stays in history.
 */
export type SaveChoice = "keep_cloud" | "keep_device" | "keep_both";

export type SaveConflictError =
  /** Already resolved (another window, or the game's launch was cancelled). */
  | { kind: "not_found" }
  /** The game is running: saves can't be replaced under it. */
  | { kind: "running" }
  /** The cloud can't be reached; nothing changed. */
  | { kind: "offline" }
  /** Another device pushed again while the dialog was open: decide again with the newer `conflict`. */
  | { kind: "head_moved"; conflict: SaveConflict }
  | { kind: "io"; detail: string };

export type SaveResolved = {
  /** The conflict came from pressing Play and the game has started now. */
  launched: boolean;
};

/** A snapshot on the server (history). `current`: it is the cloud head. */
export type SaveSnapshot = {
  snapshot_id: string;
  created_at: string;
  device_name: string;
  file_count: number;
  size_bytes: number;
  current: boolean;
};

/** Local files copied away before anything was overwritten or deleted (the 5 newest are kept). */
export type SaveBackup = {
  backup_id: string;
  created_at: string;
  file_count: number;
  size_bytes: number;
  reason: "restore" | "conflict" | "replaced";
};

export type SaveHistory = {
  /** Newest first. */
  snapshots: SaveSnapshot[];
  /** Newest first. */
  backups: SaveBackup[];
};

export type SaveSource =
  | { kind: "snapshot"; snapshot_id: string }
  | { kind: "backup"; backup_id: string };

export type SaveHistoryError =
  | { kind: "not_found" }
  /** Server snapshots can't be listed offline; `backups` are local and would still work. */
  | { kind: "offline" }
  | { kind: "io"; detail: string };

export type SaveRestoreError =
  | { kind: "not_found" }
  | { kind: "running" }
  | { kind: "offline" }
  | { kind: "io"; detail: string };

/** What a sync did, reported as it happens (before a launch and after the game exits). */
export type SaveSyncOutcome =
  /** The cloud had newer saves and they were written before launch. */
  | { kind: "restored"; file_count: number }
  /** This device's changes were pushed after the game exited. */
  | { kind: "uploaded"; file_count: number }
  /** The server couldn't be reached; the game runs on local saves and syncs later. */
  | { kind: "pending" }
  /** Both sides changed: the core needs a choice (`saves_conflict`). */
  | { kind: "conflict"; conflict_id: string }
  | { kind: "failed"; detail: string };

export type SaveSyncEvent = { package: PackageRef; title: string; outcome: SaveSyncOutcome };

/** The history of a package changed (a push, a restore, a new backup). */
export type SavesChanged = { package: PackageRef };

export const savesCommands = {
  /** The two sides of an open conflict. */
  async savesConflict(conflictId: string): Promise<Result<SaveConflict, SaveConflictError>> {
    return call("saves_conflict", { conflictId });
  },
  /** Applies the player's choice; cancelling is simply closing the dialog (nothing is sent). */
  async savesResolve(
    conflictId: string,
    choice: SaveChoice,
  ): Promise<Result<SaveResolved, SaveConflictError>> {
    return call("saves_resolve", { conflictId, choice });
  },
  async savesHistory(pkg: PackageRef): Promise<Result<SaveHistory, SaveHistoryError>> {
    return call("saves_history", { package: pkg });
  },
  /** Backs up the current local saves, then writes the chosen copy. */
  async savesRestore(pkg: PackageRef, source: SaveSource): Promise<Result<null, SaveRestoreError>> {
    return call("saves_restore", { package: pkg, source });
  },
};

export const savesEvents = makeEvents<{ saveSync: SaveSyncEvent; savesChanged: SavesChanged }>({
  saveSync: "save-sync",
  savesChanged: "saves-changed",
});
