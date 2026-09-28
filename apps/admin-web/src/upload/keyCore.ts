// The key worker's logic (01-security §3.3): the key file is decrypted inside the worker (WASM memory
// with pack-wasm) and never leaves it. The only operation offered afterwards is signing a digest.
import type { KeyLib, KeyReply, KeyRequest } from "./types";

export interface KeyState {
  lib: KeyLib;
  key: ReturnType<KeyLib["unlock"]> | null;
}

export function handleKeyRequest(state: KeyState, req: KeyRequest): KeyReply {
  switch (req.kind) {
    case "unlock": {
      let info: { kind: string; keyId: string; label: string };
      try {
        info = state.lib.keyfileInfo(req.keyfile);
      } catch {
        return { kind: "error", code: "invalid_keyfile", message: "This isn't a vgames key file." };
      }
      if (info.kind !== "publisher")
        return {
          kind: "error",
          code: "root_key",
          message:
            "This is the server's root key. Uploads are signed with a publisher key; keep the root key offline.",
        };
      try {
        state.key?.free();
        state.key = state.lib.unlock(req.keyfile, req.passphrase);
      } catch (e) {
        const message = e instanceof Error ? e.message : String(e);
        return /passphrase|decrypt|mac/i.test(message)
          ? { kind: "error", code: "wrong_passphrase", message: "The passphrase is wrong." }
          : { kind: "error", code: "invalid_keyfile", message: "The key file can't be read." };
      }
      // Best effort: the passphrase is dropped here; the bytes are wiped.
      req.keyfile.fill(0);
      return { kind: "unlocked", keyId: info.keyId, label: info.label };
    }
    case "sign": {
      if (!state.key) return { kind: "error", code: "locked", message: "Unlock the key first." };
      try {
        const envelope =
          req.context === "manifest"
            ? state.key.signManifestDigest(req.digest)
            : state.key.signCompatDigest(req.digest);
        return { kind: "signed", envelope };
      } catch (e) {
        return {
          kind: "error",
          code: "internal",
          message: e instanceof Error ? e.message : String(e),
        };
      }
    }
  }
}
