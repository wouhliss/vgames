// Mock-mode stand-ins for pack-wasm: the same chunk/pack layout as crates/vgames-pack (4 MiB chunks,
// small files sharing a chunk, packs up to the pack size), a fake 64-hex "hash", and a JSON key file
// (mocks/keyfile.ts). Only used when the real WASM module isn't built, in mock mode and in tests.
import { checkTree } from "./pathRules";
import type { Execution, KeyLib, ManifestIdentity, Packer, PackLib, PlannedFile } from "./types";

export const CHUNK_SIZE = 4 * 1024 * 1024;
export const PACK_SIZE = 256 * 1024 * 1024;

/** FNV-1a over four seeds, as 64 hex characters. Not a real hash: mock mode only. */
export function fakeHash(bytes: Uint8Array): string {
  let out = "";
  for (const seed of [0x811c9dc5, 0x01000193, 0x2b3c4d5e, 0x6f7a8b9c]) {
    let h = seed >>> 0;
    for (let i = 0; i < bytes.length; i += 1) {
      h ^= bytes[i] ?? 0;
      h = Math.imul(h, 0x01000193) >>> 0;
    }
    out += h.toString(16).padStart(8, "0").repeat(2);
  }
  return out;
}

interface Chunk {
  len: number;
  extents: { file: number; offset: number; len: number }[];
}

const utf8 = new TextEncoder();
function byteCompare(a: string, b: string): number {
  const x = utf8.encode(a);
  const y = utf8.encode(b);
  for (let i = 0; i < Math.min(x.length, y.length); i += 1) {
    const d = (x[i] ?? 0) - (y[i] ?? 0);
    if (d !== 0) return d;
  }
  return x.length - y.length;
}

export function mockPackLib(packSize = PACK_SIZE): PackLib {
  return {
    hashHex: fakeHash,
    plan(input: PlannedFile[], directories: string[]): Packer {
      const invalid = checkTree(
        input.map((f) => f.path),
        directories,
      );
      if (invalid.length > 0) throw new Error(invalid.map((i) => i.path).join("\n"));
      const files = [...input].sort((a, b) => byteCompare(a.path, b.path));
      const chunks: Chunk[] = [];
      let open: Chunk | null = null;
      files.forEach((f, file) => {
        if (f.size === 0) return;
        if (f.size >= CHUNK_SIZE) {
          if (open) chunks.push(open);
          open = null;
          for (let offset = 0; offset < f.size; offset += CHUNK_SIZE) {
            const len = Math.min(CHUNK_SIZE, f.size - offset);
            chunks.push({ len, extents: [{ file, offset, len }] });
          }
          return;
        }
        if (open && open.len + f.size > CHUNK_SIZE) {
          chunks.push(open);
          open = null;
        }
        open ??= { len: 0, extents: [] };
        open.extents.push({ file, offset: 0, len: f.size });
        open.len += f.size;
      });
      if (open) chunks.push(open);
      const packs: { first: number; count: number; size: number }[] = [];
      chunks.forEach((c, i) => {
        const last = packs[packs.length - 1];
        if (!last || last.size + c.len > packSize) packs.push({ first: i, count: 1, size: c.len });
        else {
          last.count += 1;
          last.size += c.len;
        }
      });
      const next = new Map<number, number>();
      return {
        files: () => files,
        packCount: () => packs.length,
        packSize: (p) => packs[p]?.size,
        packChunks: (p) => {
          const pack = packs[p];
          return pack ? Array.from({ length: pack.count }, (_, i) => pack.first + i) : [];
        },
        chunkExtents: (c) => chunks[c]?.extents ?? [],
        addChunk(c, bytes) {
          const chunk = chunks[c];
          if (!chunk || bytes.length !== chunk.len)
            throw new Error(`chunk ${c}: expected ${chunk?.len} bytes`);
          const p = packs.findIndex((pk) => c >= pk.first && c < pk.first + pk.count);
          const pack = packs[p];
          if (!pack) throw new Error("no such pack");
          if (c === pack.first) next.set(p, pack.first);
          if (next.get(p) !== c)
            throw new Error(`pack ${p}: expected chunk ${next.get(p)}, got ${c}`);
          next.set(p, c + 1);
        },
        buildManifest(identity: ManifestIdentity, execution: Execution | undefined) {
          const manifest = {
            format: "vgames.manifest/1",
            ...identity,
            files: files.map((f) => ({ path: f.path, size: f.size })),
            packs: packs.map((p, index) => ({ index, size: p.size })),
            ...(execution ?? {}),
          };
          return utf8.encode(JSON.stringify(manifest));
        },
      };
    },
  };
}

interface MockKeyFileJson {
  format?: string;
  kind?: string;
  key_id?: string;
  label?: string;
  passphrase?: string;
}

export const mockKeyLib: KeyLib = {
  keyfileInfo(bytes) {
    let parsed: MockKeyFileJson;
    try {
      parsed = JSON.parse(new TextDecoder().decode(bytes)) as MockKeyFileJson;
    } catch {
      throw new Error("not a vgames key file");
    }
    if (parsed.format !== "vgames.key/mock" || !parsed.key_id)
      throw new Error("not a vgames key file");
    return { kind: parsed.kind ?? "publisher", keyId: parsed.key_id, label: parsed.label ?? "" };
  },
  unlock(bytes, passphrase) {
    const info = mockKeyLib.keyfileInfo(bytes);
    const parsed = JSON.parse(new TextDecoder().decode(bytes)) as MockKeyFileJson;
    if (info.kind !== "publisher")
      throw new Error(
        "this is a root key: sign trust bundles with the offline CLI, not in a browser",
      );
    if (parsed.passphrase !== passphrase) throw new Error("wrong passphrase");
    let freed = false;
    const sign = (context: string, hex: string) => {
      if (freed) throw new Error("key freed");
      if (!/^[0-9a-f]{64}$/.test(hex)) throw new Error("bad digest");
      return JSON.stringify({
        format: "vgames.sig/1",
        alg: "ed25519",
        context,
        key_id: info.keyId,
        payload_blake3: hex,
        signature: btoa(`mock-signature:${hex}`),
      });
    };
    return {
      keyId: info.keyId,
      signManifestDigest: (hex) => sign("vgames/manifest/v1", hex),
      signCompatDigest: (hex) => sign("vgames/compat/v1", hex),
      free: () => {
        freed = true;
      },
    };
  },
};
