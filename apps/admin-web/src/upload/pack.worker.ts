/// <reference lib="webworker" />
// The pack worker: plans the folder and streams pack bytes, off the main thread.
import { libraries } from "./libs";
import { handlePackRequest, type PackState, packState } from "./packCore";
import type { PackReply, PackRequest } from "./types";

declare const self: DedicatedWorkerGlobalScope;

let state: Promise<PackState | null> | null = null;

self.onmessage = async (
  e: MessageEvent<{ id: number; req: PackRequest & { mockPackSize?: number } }>,
) => {
  const { id, req } = e.data;
  state ??= libraries({ mockPackSize: req.kind === "plan" ? req.mockPackSize : undefined }).then(
    (libs) => (libs ? packState(libs.pack) : null),
  );
  const s = await state;
  let reply: PackReply;
  if (!s) {
    reply = {
      kind: "error",
      code: "wasm_missing",
      message:
        "The uploader's packing module is missing from this build. Ask the server owner to rebuild the admin pages.",
    };
  } else {
    reply = await handlePackRequest(s, req);
  }
  const transfer = reply.kind === "piece" || reply.kind === "manifest" ? [reply.bytes.buffer] : [];
  self.postMessage({ id, reply }, transfer as Transferable[]);
};
