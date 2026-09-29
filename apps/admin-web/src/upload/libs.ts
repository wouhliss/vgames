// Where the packer and signer come from. pack-wasm (packages/pack-wasm, built by `wasm-pack`, not
// checked in) is bundled when it was built before `vite build`; the optional glob keeps type checks
// and builds working without it. Mock mode and tests fall back to the stand-ins in mockPack.ts;
// production refuses to upload without the real module.
import { mockKeyLib, mockPackLib } from "./mockPack";
import type { KeyLib, Packer, PackLib, PlannedFile } from "./types";

interface WasmModule {
  default: () => Promise<unknown>;
  WasmPacker: { plan(files: PlannedFile[], directories: string[]): Packer };
  Blake3Hasher: new () => { update(b: Uint8Array): void; finalizeHex(): string; free(): void };
  keyfileInfo(bytes: Uint8Array): { kind: string; keyId: string; label: string; free?(): void };
  UnlockedKey: { unlock(bytes: Uint8Array, passphrase: string): ReturnType<KeyLib["unlock"]> };
}

const modules = import.meta.glob<WasmModule>("../../../../packages/pack-wasm/pkg/vgames_pack.js");

declare const __VGAMES_MOCK_UPLOADS__: boolean;

/** Mock mode and tests (a build-time constant, so production bundles drop the stand-ins). */
const mockAllowed = __VGAMES_MOCK_UPLOADS__;

let loaded: Promise<{ pack: PackLib; key: KeyLib } | null> | null = null;

async function loadWasm(): Promise<{ pack: PackLib; key: KeyLib } | null> {
  const load = Object.values(modules)[0];
  if (!load) return null;
  const mod = await load();
  await mod.default();
  return {
    pack: {
      plan: (files, dirs) => mod.WasmPacker.plan(files, dirs),
      hashHex(bytes) {
        const h = new mod.Blake3Hasher();
        try {
          h.update(bytes);
          return h.finalizeHex();
        } finally {
          h.free();
        }
      },
    },
    key: {
      keyfileInfo(bytes) {
        const info = mod.keyfileInfo(bytes);
        const out = { kind: info.kind, keyId: info.keyId, label: info.label };
        info.free?.();
        return out;
      },
      unlock: (bytes, passphrase) => mod.UnlockedKey.unlock(bytes, passphrase),
    },
  };
}

/**
 * The real libraries, or in mock mode and tests the stand-ins (the mock server can't check real
 * signatures, and the mock key file isn't a real one). Null when pack-wasm is missing.
 */
export async function libraries(
  options: { mockPackSize?: number | undefined } = {},
): Promise<{ pack: PackLib; key: KeyLib } | null> {
  if (mockAllowed) return { pack: mockPackLib(options.mockPackSize), key: mockKeyLib };
  loaded ??= loadWasm().catch(() => null);
  return loaded;
}
