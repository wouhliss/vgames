// Which save conflict the dialog shows. The launch toast's "Resolve" action and the `save-conflict`
// event both ask for one; the dialog closes it. One at a time: a newer request replaces the shown one.
import { useSyncExternalStore } from "react";

let requested: string | null = null;
const listeners = new Set<() => void>();

function set(next: string | null): void {
  requested = next;
  for (const listener of listeners) listener();
}

export function requestSaveConflict(conflictId: string): void {
  set(conflictId);
}

export function closeSaveConflict(): void {
  set(null);
}

export function useRequestedSaveConflict(): string | null {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => requested,
  );
}
