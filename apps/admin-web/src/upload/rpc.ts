// Request/response over a Worker (or an in-process stand-in in tests), matched by id.
import type { KeyReply, KeyRequest, PackReply, PackRequest } from "./types";

export interface Channel<Req, Rep> {
  call(req: Req, transfer?: Transferable[]): Promise<Rep>;
  terminate(): void;
}

export function workerChannel<Req, Rep>(worker: Worker): Channel<Req, Rep> {
  let seq = 0;
  const pending = new Map<number, { resolve: (r: Rep) => void; reject: (e: unknown) => void }>();
  worker.onmessage = (e: MessageEvent<{ id: number; reply: Rep }>) => {
    const entry = pending.get(e.data.id);
    pending.delete(e.data.id);
    entry?.resolve(e.data.reply);
  };
  worker.onerror = (e) => {
    for (const entry of pending.values()) entry.reject(new Error(e.message || "worker failed"));
    pending.clear();
  };
  return {
    call(req, transfer = []) {
      seq += 1;
      const id = seq;
      return new Promise<Rep>((resolve, reject) => {
        pending.set(id, { resolve, reject });
        worker.postMessage({ id, req }, transfer);
      });
    },
    terminate() {
      worker.terminate();
      for (const entry of pending.values()) entry.reject(new Error("worker terminated"));
      pending.clear();
    },
  };
}

export type PackChannel = Channel<PackRequest & { mockPackSize?: number }, PackReply>;
export type KeyChannel = Channel<KeyRequest, KeyReply>;

export interface Workers {
  pack: () => PackChannel;
  key: () => KeyChannel;
}

/** Real workers (the browser). */
export const browserWorkers: Workers = {
  pack: () =>
    workerChannel(
      new Worker(new URL("./pack.worker.ts", import.meta.url), {
        type: "module",
        name: "vgames-pack",
      }),
    ),
  key: () =>
    workerChannel(
      new Worker(new URL("./key.worker.ts", import.meta.url), {
        type: "module",
        name: "vgames-key",
      }),
    ),
};
