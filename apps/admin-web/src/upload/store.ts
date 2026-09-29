// What a resumable upload needs after a reload (A3-T16): the version, the planned files (to check the
// re-picked folder is unchanged), and per pack its session URI and confirmed offset. IndexedDB, with
// an in-memory fallback where it doesn't exist (tests, some private windows). Never the key or the
// passphrase.
import type { Execution, PlannedFile } from "./types";

export interface SavedUpload {
  versionId: string;
  packageId: string;
  serverId: string;
  platform: string;
  label: string;
  sequence: number;
  /** Unix seconds, from the version's `created_at`. */
  createdAt: number;
  files: PlannedFile[];
  directories: string[];
  packs: number[];
  execution: Execution | undefined;
  sessions: Record<number, string>;
  confirmed: Record<number, number>;
  complete: number[];
  savedAt: string;
}

export interface UploadStore {
  get(versionId: string): Promise<SavedUpload | null>;
  put(saved: SavedUpload): Promise<void>;
  delete(versionId: string): Promise<void>;
  list(): Promise<SavedUpload[]>;
}

export function memoryStore(): UploadStore {
  const data = new Map<string, SavedUpload>();
  return {
    get: async (id) => structuredClone(data.get(id) ?? null),
    put: async (s) => {
      data.set(s.versionId, structuredClone(s));
    },
    delete: async (id) => {
      data.delete(id);
    },
    list: async () => [...data.values()].map((s) => structuredClone(s)),
  };
}

const DB = "vgames-admin-uploads";
const STORE = "uploads";

function request<T>(r: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });
}

export function indexedDbStore(): UploadStore {
  let opened: Promise<IDBDatabase> | null = null;
  const db = () => {
    opened ??= new Promise((resolve, reject) => {
      const open = indexedDB.open(DB, 1);
      open.onupgradeneeded = () => open.result.createObjectStore(STORE, { keyPath: "versionId" });
      open.onsuccess = () => resolve(open.result);
      open.onerror = () => reject(open.error);
    });
    return opened;
  };
  const tx = async (mode: IDBTransactionMode) =>
    (await db()).transaction(STORE, mode).objectStore(STORE);
  return {
    get: async (id) =>
      ((await request((await tx("readonly")).get(id))) as SavedUpload | undefined) ?? null,
    put: async (s) => {
      await request((await tx("readwrite")).put(s));
    },
    delete: async (id) => {
      await request((await tx("readwrite")).delete(id));
    },
    list: async () => (await request((await tx("readonly")).getAll())) as SavedUpload[],
  };
}

let shared: UploadStore | null = null;

export function uploadStore(): UploadStore {
  shared ??= typeof indexedDB === "undefined" ? memoryStore() : indexedDbStore();
  return shared;
}

/** Test hook: replace the shared store. */
export function setUploadStore(store: UploadStore | null): void {
  shared = store;
}

/** Differences between the saved plan and a re-picked folder (empty = same). */
export function folderChanges(
  saved: readonly PlannedFile[],
  picked: readonly PlannedFile[],
): string[] {
  const now = new Map(picked.map((f) => [f.path, f]));
  const out: string[] = [];
  for (const f of saved) {
    const p = now.get(f.path);
    if (!p) out.push(`${f.path} is missing`);
    else if (p.size !== f.size) out.push(`${f.path} changed size`);
    else if (p.mtime_ms !== f.mtime_ms) out.push(`${f.path} was modified`);
    now.delete(f.path);
  }
  for (const path of now.keys()) out.push(`${path} is new`);
  return out;
}
