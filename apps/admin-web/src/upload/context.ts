// What the upload wizard runs on: real workers and IndexedDB in the browser; tests swap in
// in-process workers and a memory store.
import { createContext, useContext } from "react";
import { browserWorkers, type Workers } from "./rpc";
import { type UploadStore, uploadStore } from "./store";

export interface UploadDeps {
  workers: Workers;
  store: () => UploadStore;
  /** Mock mode only: smaller packs so a small folder still shows several. */
  mockPackSize?: number | undefined;
}

export const UploadDepsContext = createContext<UploadDeps>({
  workers: browserWorkers,
  store: uploadStore,
  // Mock mode: 8 MiB packs, so a small demo folder shows several packs.
  mockPackSize: import.meta.env.MODE === "mock" ? 8 * 1024 * 1024 : undefined,
});

export const useUploadDeps = () => useContext(UploadDepsContext);
