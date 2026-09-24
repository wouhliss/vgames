// Smoke test of the generated WASM package in Node: plan → add chunks → manifest.
// Run after `pnpm --filter @vgames/pack-wasm build` with `pnpm --filter @vgames/pack-wasm smoke`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { Blake3Hasher, initSync, WasmPacker } from "../pkg/vgames_pack.js";

initSync({ module: readFileSync(new URL("../pkg/vgames_pack_bg.wasm", import.meta.url)) });

const CHUNK = 4 * 1024 * 1024;
const contents = [
  new Uint8Array(CHUNK + 10).fill(7),
  new TextEncoder().encode("hello"),
  new Uint8Array(0),
];
const files = [
  { path: "Game/big.bin", size: contents[0].length, mtime_ms: 1 },
  { path: "Game/readme.txt", size: 5, mtime_ms: 2 },
  { path: "Game/empty.flag", size: 0, mtime_ms: 3 },
];
const packer = WasmPacker.plan(files, ["Game/Saved"]);
const planned = packer.files();
const data = new Map(files.map((f, i) => [f.path, contents[i]]));

for (let p = 0; p < packer.packCount(); p++) {
  for (const c of packer.packChunks(p)) {
    const parts = packer
      .chunkExtents(c)
      .map(({ file, offset, len }) => data.get(planned[file].path).subarray(offset, offset + len));
    const bytes = new Uint8Array(parts.reduce((n, a) => n + a.length, 0));
    let at = 0;
    for (const part of parts) {
      bytes.set(part, at);
      at += part.length;
    }
    packer.addChunk(c, bytes);
  }
}

const manifest = JSON.parse(
  new TextDecoder().decode(
    packer.buildManifest(
      {
        server_id: "01920000-0000-7000-8000-000000000000",
        package_id: "0192a6f0-1c2d-7e3f-8a9b-0c1d2e3f4a5b",
        version_id: "0192a6f1-aaaa-7bbb-8ccc-dddddddddddd",
        sequence: 1,
        version_label: "1.0",
        platform: "windows-x86_64",
        created_at: 1790244000,
      },
      undefined,
    ),
  ),
);
assert.equal(manifest.totals.files, 3);
assert.equal(manifest.totals.chunks, 3);
assert.deepEqual(manifest.directories, ["Game/Saved"]);
const hasher = new Blake3Hasher();
hasher.update(contents[1]);
assert.equal(manifest.files.find((f) => f.path === "Game/readme.txt").blake3, hasher.finalizeHex());
assert.throws(() => WasmPacker.plan([{ path: "CON", size: 1, mtime_ms: 0 }], []), /reserved/);
console.log("pack-wasm smoke test passed");
