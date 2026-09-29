// One version upload (02-package-format §6), driven from the page: packs go up in parallel through
// GCS resumable sessions while the pack worker produces their bytes; progress and sessions are saved
// so a reload can resume; the manifest is signed in the key worker, uploaded, and finalized; then the
// server's verification is followed until the version is ready.
//
// Recovery: a lost network waits for the connection and resumes on its own; an expired start URL
// gets a new one; a vanished session starts that pack again; 5xx retries with backoff.
import { ApiError } from "../api/errors";
import type { Version } from "../api/schemas";
import {
  FINALIZE_ERRORS,
  finalizeVersion,
  getVersion,
  manifestUpload,
  packUploadSession,
} from "../api/versions";
import { GcsError, putObject, putPiece, queryOffset, startSession } from "./gcs";
import type { KeyChannel, PackChannel } from "./rpc";
import type { SavedUpload, UploadStore } from "./store";

export const CONCURRENCY = 4;
export const VERIFY_POLL_MS = 2000;
const MAX_BACKOFF_MS = 30_000;

export type PackStatus = "waiting" | "uploading" | "done";

export interface PackProgress {
  index: number;
  size: number;
  sent: number;
  status: PackStatus;
}

export type Phase =
  | { kind: "ready" }
  | { kind: "uploading" }
  | { kind: "paused" }
  | { kind: "waiting_network"; retryAt: number }
  | { kind: "signing" }
  | { kind: "finalizing" }
  | { kind: "verifying"; progress: number }
  | { kind: "verified"; version: Version }
  | { kind: "failed"; title: string; text: string; retryable: boolean };

export interface Snapshot {
  phase: Phase;
  packs: PackProgress[];
  sent: number;
  total: number;
}

export interface EngineDeps {
  packs: PackChannel;
  keys: KeyChannel;
  store: UploadStore;
  /** Waits for the network (the `online` event) or a timeout; injectable for tests. */
  waitForNetwork?: (ms: number, signal: AbortSignal) => Promise<void>;
  sleep?: (ms: number, signal: AbortSignal) => Promise<void>;
}

class Paused extends Error {}

function defaultSleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(resolve, ms);
    signal.addEventListener(
      "abort",
      () => {
        clearTimeout(timer);
        reject(new Paused());
      },
      { once: true },
    );
  });
}

function defaultWaitForNetwork(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const done = () => {
      clearTimeout(timer);
      window.removeEventListener("online", done);
      resolve();
    };
    const timer = setTimeout(done, ms);
    window.addEventListener("online", done);
    signal.addEventListener(
      "abort",
      () => {
        clearTimeout(timer);
        window.removeEventListener("online", done);
        reject(new Paused());
      },
      { once: true },
    );
  });
}

export class UploadEngine {
  private snapshot: Snapshot;
  private listeners = new Set<() => void>();
  private controller: AbortController | null = null;
  private running: Promise<void> | null = null;
  private readonly sleep: NonNullable<EngineDeps["sleep"]>;
  private readonly waitForNetwork: NonNullable<EngineDeps["waitForNetwork"]>;

  constructor(
    private saved: SavedUpload,
    private deps: EngineDeps,
  ) {
    this.sleep = deps.sleep ?? defaultSleep;
    this.waitForNetwork = deps.waitForNetwork ?? defaultWaitForNetwork;
    const packs = saved.packs.map((size, index) => ({
      index,
      size,
      sent: saved.complete.includes(index) ? size : (saved.confirmed[index] ?? 0),
      status: (saved.complete.includes(index) ? "done" : "waiting") as PackStatus,
    }));
    this.snapshot = { phase: { kind: "ready" }, packs, sent: 0, total: 0 };
    this.recount();
  }

  // ---- observable state (useSyncExternalStore) ----------------------------------------------

  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  getSnapshot = () => this.snapshot;

  private set(patch: Partial<Snapshot>) {
    this.snapshot = { ...this.snapshot, ...patch };
    for (const l of this.listeners) l();
  }

  private recount() {
    const packs = this.snapshot.packs;
    this.set({
      sent: packs.reduce((n, p) => n + p.sent, 0),
      total: packs.reduce((n, p) => n + p.size, 0),
    });
  }

  private pack(index: number, patch: Partial<PackProgress>) {
    this.snapshot = {
      ...this.snapshot,
      packs: this.snapshot.packs.map((p) => (p.index === index ? { ...p, ...patch } : p)),
    };
    this.recount();
  }

  get active(): boolean {
    const k = this.snapshot.phase.kind;
    return k === "uploading" || k === "waiting_network" || k === "signing" || k === "finalizing";
  }

  // ---- control --------------------------------------------------------------------------------

  /** Starts or resumes. Resolves when the run stops (done, paused or failed). */
  start(): Promise<void> {
    if (this.running) return this.running;
    this.controller = new AbortController();
    const signal = this.controller.signal;
    this.running = this.run(signal).finally(() => {
      this.running = null;
      this.controller = null;
    });
    return this.running;
  }

  /** Stops after the pieces in flight; progress is kept. */
  pause(): void {
    this.controller?.abort();
  }

  private async save() {
    this.saved.savedAt = new Date().toISOString();
    await this.deps.store.put(this.saved);
  }

  private async run(signal: AbortSignal) {
    try {
      if (this.saved.complete.length < this.saved.packs.length) {
        this.set({ phase: { kind: "uploading" } });
        await this.uploadPacks(signal);
      }
      await this.finish(signal);
    } catch (e) {
      if (e instanceof Paused || signal.aborted) {
        this.set({ phase: { kind: "paused" } });
        for (const p of this.snapshot.packs)
          if (p.status === "uploading") this.pack(p.index, { status: "waiting" });
        return;
      }
      // Packs the server says are missing or wrong are checked again on the next try.
      if (e instanceof ApiError && (e.code === "pack_missing" || e.code === "pack_size_mismatch")) {
        this.saved.complete = [];
        for (const p of this.snapshot.packs) this.pack(p.index, { status: "waiting" });
        await this.save();
      }
      this.set({ phase: failure(e) });
    }
  }

  /** Uses another key (e.g. after "key not trusted"): the old worker is dropped. */
  replaceKeys(keys: KeyChannel): void {
    this.deps.keys.terminate();
    this.deps.keys = keys;
  }

  private async uploadPacks(signal: AbortSignal) {
    const queue = this.snapshot.packs.filter((p) => p.status !== "done").map((p) => p.index);
    const worker = async () => {
      for (let next = queue.shift(); next !== undefined; next = queue.shift()) {
        await this.withRecovery(signal, () => this.uploadPack(next, signal));
      }
    };
    await Promise.all(Array.from({ length: Math.min(CONCURRENCY, queue.length) }, worker));
  }

  /** Retries transient failures: waits for the network, backs off on 5xx. */
  private async withRecovery(signal: AbortSignal, step: () => Promise<void>) {
    let attempt = 0;
    for (;;) {
      if (signal.aborted) throw new Paused();
      try {
        await step();
        if (this.snapshot.phase.kind === "waiting_network")
          this.set({ phase: { kind: "uploading" } });
        return;
      } catch (e) {
        if (e instanceof Paused || signal.aborted) throw new Paused();
        const network =
          (e instanceof GcsError && e.kind === "network") ||
          (e instanceof ApiError && (e.detail.kind === "network" || e.detail.kind === "timeout"));
        const server =
          (e instanceof GcsError && e.kind === "server") || (e instanceof ApiError && e.transient);
        if (!network && !server) throw e;
        attempt += 1;
        const wait = Math.min(MAX_BACKOFF_MS, 1000 * 2 ** Math.min(attempt, 5));
        if (network) {
          this.set({ phase: { kind: "waiting_network", retryAt: Date.now() + wait } });
          await this.waitForNetwork(wait, signal);
          this.set({ phase: { kind: "uploading" } });
        } else {
          await this.sleep(wait, signal);
        }
      }
    }
  }

  private async session(index: number, signal: AbortSignal, fresh = false): Promise<string> {
    const known = this.saved.sessions[index];
    if (known && !fresh) return known;
    // A start URL is valid for 15 minutes; an expired one is simply requested again.
    for (let attempt = 0; ; attempt += 1) {
      const { data: target } = await packUploadSession(this.saved.versionId, index);
      try {
        const uri = await startSession(target, signal);
        this.saved.sessions[index] = uri;
        this.saved.confirmed[index] = 0;
        await this.save();
        return uri;
      } catch (e) {
        if (e instanceof GcsError && e.kind === "expired" && attempt < 2) continue;
        throw e;
      }
    }
  }

  private async uploadPack(index: number, signal: AbortSignal) {
    const size = this.saved.packs[index] ?? 0;
    this.pack(index, { status: "uploading" });
    let uri = await this.session(index, signal);
    let offset: number;
    try {
      offset = await queryOffset(uri, size, signal);
    } catch (e) {
      if (!(e instanceof GcsError && e.kind === "gone")) throw e;
      uri = await this.session(index, signal, true);
      offset = 0;
    }
    this.pack(index, { sent: offset });
    if (offset < size) {
      const opened = await this.deps.packs.call({ kind: "open", pack: index, from: offset });
      if (opened.kind === "error") throw new PackFailure(opened.code, opened.message);
      for (;;) {
        if (signal.aborted) throw new Paused();
        const piece = await this.deps.packs.call({ kind: "next", pack: index });
        if (piece.kind === "error") throw new PackFailure(piece.code, piece.message);
        if (piece.kind !== "piece") throw new PackFailure("internal", "unexpected reply");
        if (piece.bytes.length > 0) {
          const confirmedAt = await putPiece(uri, piece.bytes, piece.offset, size, signal);
          this.saved.confirmed[index] = confirmedAt;
          this.pack(index, { sent: confirmedAt });
          await this.save();
          // The server didn't take all of it (it keeps 256 KiB multiples): continue from there.
          if (confirmedAt < piece.offset + piece.bytes.length) {
            const again = await this.deps.packs.call({
              kind: "open",
              pack: index,
              from: confirmedAt,
            });
            if (again.kind === "error") throw new PackFailure(again.code, again.message);
            continue;
          }
        }
        if (piece.last) break;
      }
      offset = await queryOffset(uri, size, signal);
      if (offset < size) throw new GcsError("server", null);
    }
    this.saved.complete = [...new Set([...this.saved.complete, index])];
    await this.save();
    this.pack(index, { sent: size, status: "done" });
  }

  private async finish(signal: AbortSignal) {
    this.set({ phase: { kind: "signing" } });
    const built = await this.deps.packs.call({
      kind: "manifest",
      identity: {
        server_id: this.saved.serverId,
        package_id: this.saved.packageId,
        version_id: this.saved.versionId,
        sequence: this.saved.sequence,
        version_label: this.saved.label,
        platform: this.saved.platform,
        created_at: this.saved.createdAt,
      },
      execution: this.saved.execution,
    });
    if (built.kind !== "manifest")
      throw new PackFailure(
        built.kind === "error" ? built.code : "internal",
        built.kind === "error" ? built.message : "no manifest",
      );
    const signed = await this.deps.keys.call({
      kind: "sign",
      digest: built.blake3,
      context: "manifest",
    });
    if (signed.kind !== "signed")
      throw new KeyFailure(signed.kind === "error" ? signed.message : "signing failed");
    this.set({ phase: { kind: "finalizing" } });
    await this.withRecovery(signal, async () => {
      const { data: target } = await manifestUpload(this.saved.versionId);
      await putObject(target, built.bytes, signal);
    });
    await finalizeVersion(this.saved.versionId, {
      manifest_size: built.bytes.length,
      manifest_blake3: built.blake3,
      signature: JSON.parse(signed.envelope),
    });
    // The key is no longer needed: drop the worker (and the decrypted key with it).
    this.deps.keys.terminate();
    await this.deps.store.delete(this.saved.versionId);
    await this.follow(signal);
  }

  /** Follows the server's verification until the version is ready (or failed). */
  async follow(signal?: AbortSignal) {
    const stop = signal ?? new AbortController().signal;
    for (;;) {
      let version!: Version;
      await this.withRecovery(stop, async () => {
        version = (await getVersion(this.saved.versionId)).data;
      });
      if (version.state === "verifying") {
        this.set({ phase: { kind: "verifying", progress: version.verify_progress ?? 0 } });
        await this.sleep(VERIFY_POLL_MS, stop);
        continue;
      }
      if (version.state === "failed")
        this.set({
          phase: {
            kind: "failed",
            title: "Verification failed",
            text: `The server checked the uploaded files and found a problem: ${version.failure_reason ?? "unknown"}. Start a new upload.`,
            retryable: false,
          },
        });
      else this.set({ phase: { kind: "verified", version } });
      return;
    }
  }
}

export class PackFailure extends Error {
  constructor(
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

class KeyFailure extends Error {}

function failure(e: unknown): Phase {
  if (e instanceof PackFailure)
    return {
      kind: "failed",
      title: e.code === "file_changed" ? "A file changed" : "Reading the folder failed",
      text: e.message,
      retryable: e.code !== "wasm_missing",
    };
  if (e instanceof KeyFailure)
    return { kind: "failed", title: "Signing failed", text: e.message, retryable: false };
  if (e instanceof ApiError && e.status === 422) {
    const code = e.code ?? "";
    return {
      kind: "failed",
      title: "The server refused the version",
      text:
        FINALIZE_ERRORS[code] ??
        `${e.detail.kind === "http" ? (e.detail.problem.detail ?? e.detail.problem.title) : ""}`,
      retryable:
        code === "pack_missing" ||
        code === "pack_size_mismatch" ||
        code === "manifest_missing" ||
        code === "manifest_hash_mismatch",
    };
  }
  if (e instanceof ApiError && e.status === 409)
    return {
      kind: "failed",
      title: "This version can't be uploaded anymore",
      text: "It was finalized or aborted elsewhere. Reload the versions list.",
      retryable: false,
    };
  if (e instanceof GcsError)
    return {
      kind: "failed",
      title: "Storage refused the upload",
      text: `Cloud storage answered ${e.status ?? "with an error"}. Try again; if it keeps failing, check the server's storage settings.`,
      retryable: true,
    };
  if (e instanceof ApiError)
    return { kind: "failed", title: "The upload stopped", text: e.message, retryable: e.transient };
  return {
    kind: "failed",
    title: "The upload stopped",
    text: e instanceof Error ? e.message : String(e),
    retryable: true,
  };
}
