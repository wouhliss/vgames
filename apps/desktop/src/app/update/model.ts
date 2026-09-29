// What's new: grouping the changelog entries the Rust core selected (08-release §3.3).
import type { ChangeEntry, ChangeType, UpdaterStatus } from "../../ipc";

/** Groups in the order the dialog shows them ("New", "Improved", "Fixed", "Removed", "Security"). */
export const GROUP_ORDER: readonly ChangeType[] = [
  "added",
  "changed",
  "fixed",
  "removed",
  "security",
];

export interface EntryGroup {
  type: ChangeType;
  texts: string[];
}

/** Non-empty groups in display order; entries keep their order inside a group. */
export function groupEntries(entries: readonly ChangeEntry[]): EntryGroup[] {
  return GROUP_ORDER.map((type) => ({
    type,
    texts: entries.filter((e) => e.type === type).map((e) => e.text),
  })).filter((g) => g.texts.length > 0);
}

/** The version the banner and dialog talk about, when an update is known. */
export function updateVersion(status: UpdaterStatus | undefined): string | null {
  const state = status?.state;
  switch (state?.kind) {
    case "available":
    case "downloading":
    case "installed":
      return state.version;
    default:
      return null;
  }
}

/** Why "Install and restart" can't be used right now, if it can't. */
export type InstallBlock = "game_running" | "installing" | null;

export function installBlock(status: UpdaterStatus | undefined, pending: boolean): InstallBlock {
  if (pending) return "installing";
  const kind = status?.state.kind;
  if (kind === "downloading" || kind === "installed") return "installing";
  if (status?.blocked === "game_running") return "game_running";
  return null;
}
