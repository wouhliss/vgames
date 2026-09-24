// "Is the API reachable?" as a tiny external store. Set offline by network errors and the browser's
// offline event; set online by the online event or any successful response.
import { useSyncExternalStore } from "react";

let offline = typeof navigator !== "undefined" && navigator.onLine === false;
const listeners = new Set<() => void>();

function set(next: boolean): void {
  if (offline === next) return;
  offline = next;
  for (const l of listeners) l();
}

export const connectivity = {
  markOffline: () => set(true),
  markOnline: () => set(false),
  subscribe(listener: () => void): () => void {
    listeners.add(listener);
    return () => listeners.delete(listener);
  },
  isOffline: () => offline,
};

if (typeof window !== "undefined") {
  window.addEventListener("offline", connectivity.markOffline);
  window.addEventListener("online", connectivity.markOnline);
}

export function useOffline(): boolean {
  return useSyncExternalStore(connectivity.subscribe, connectivity.isOffline);
}
