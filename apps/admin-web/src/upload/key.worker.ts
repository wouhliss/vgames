/// <reference lib="webworker" />
// The key worker (01-security §3.3): decrypts the publisher key and signs digests. Nothing else goes
// in or out; the page terminates it once the version is finalized.
import { handleKeyRequest, type KeyState } from "./keyCore";
import { libraries } from "./libs";
import type { KeyReply, KeyRequest } from "./types";

declare const self: DedicatedWorkerGlobalScope;

let state: Promise<KeyState | null> | null = null;

self.onmessage = async (e: MessageEvent<{ id: number; req: KeyRequest }>) => {
  const { id, req } = e.data;
  state ??= libraries().then((libs) => (libs ? { lib: libs.key, key: null } : null));
  const s = await state;
  const reply: KeyReply = s
    ? handleKeyRequest(s, req)
    : {
        kind: "error",
        code: "wasm_missing",
        message:
          "The uploader's signing module is missing from this build. Ask the server owner to rebuild the admin pages.",
      };
  self.postMessage({ id, reply });
};
