// The pack worker's logic, as plain functions so tests can run it without a Worker: plan the folder,
// then stream each pack's stored bytes from any offset (hashing every chunk from the pack's start,
// as pack-wasm requires), and build the manifest at the end.
import { checkTree } from "./pathRules";
import type { Packer, PackLib, PackReply, PackRequest, PlannedFile } from "./types";

/** GCS resumable uploads take pieces in multiples of 256 KiB; 16 MiB per request (02 §6). */
export const PIECE = 16 * 1024 * 1024;

interface Cursor {
  chunks: number[];
  next: number;
  /** Stored bytes of the pack produced so far (all chunks, before `from` too). */
  produced: number;
  from: number;
  buffer: Uint8Array[];
  buffered: number;
}

export interface PackState {
  lib: PackLib;
  packer: Packer | null;
  /** Blobs in the packer's file order. */
  blobs: File[];
  planned: PlannedFile[];
  cursors: Map<number, Cursor>;
}

export function packState(lib: PackLib): PackState {
  return { lib, packer: null, blobs: [], planned: [], cursors: new Map() };
}

async function readChunk(state: PackState, chunk: number): Promise<Uint8Array> {
  const packer = state.packer;
  if (!packer) throw new Error("no plan");
  const extents = packer.chunkExtents(chunk);
  const parts: Uint8Array[] = [];
  let total = 0;
  for (const e of extents) {
    const blob = state.blobs[e.file];
    const planned = state.planned[e.file];
    if (!blob || !planned) throw new Error(`no file ${e.file}`);
    if (blob.size !== planned.size || blob.lastModified !== planned.mtime_ms)
      throw new FileChanged(planned.path);
    const offset = Number(e.offset);
    const len = Number(e.len);
    let bytes: Uint8Array;
    try {
      bytes = new Uint8Array(await blob.slice(offset, offset + len).arrayBuffer());
    } catch {
      // Chrome refuses to read a file that changed on disk since it was picked.
      throw new FileChanged(planned.path);
    }
    if (bytes.length !== len) throw new FileChanged(planned.path);
    parts.push(bytes);
    total += len;
  }
  if (parts.length === 1 && parts[0]) return parts[0];
  const out = new Uint8Array(total);
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

class FileChanged extends Error {}

function concat(parts: Uint8Array[], size: number): Uint8Array {
  const out = new Uint8Array(size);
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

export async function handlePackRequest(state: PackState, req: PackRequest): Promise<PackReply> {
  try {
    switch (req.kind) {
      case "plan": {
        const invalid = checkTree(
          req.files.map((f) => f.path),
          req.directories,
        );
        if (invalid.length > 0)
          return { kind: "invalid", paths: invalid.map((i) => `${i.path}: ${i.reason}`) };
        let packer: Packer;
        try {
          packer = state.lib.plan(req.files, req.directories);
        } catch (e) {
          return { kind: "invalid", paths: String(e instanceof Error ? e.message : e).split("\n") };
        }
        state.packer?.free?.();
        state.packer = packer;
        state.planned = packer.files();
        const byPath = new Map(req.files.map((f, i) => [f.path, req.blobs[i]]));
        state.blobs = state.planned.map((f) => {
          const blob = byPath.get(f.path);
          if (!blob) throw new Error(`no data for ${f.path}`);
          return blob;
        });
        state.cursors.clear();
        const packs = Array.from({ length: packer.packCount() }, (_, p) => packer.packSize(p) ?? 0);
        return {
          kind: "planned",
          summary: {
            files: state.planned,
            packs,
            totalBytes: state.planned.reduce((n, f) => n + f.size, 0),
          },
        };
      }
      case "open": {
        const packer = state.packer;
        if (!packer) return { kind: "error", code: "internal", message: "no plan" };
        state.cursors.set(req.pack, {
          chunks: [...packer.packChunks(req.pack)],
          next: 0,
          produced: 0,
          from: req.from,
          buffer: [],
          buffered: 0,
        });
        return { kind: "opened" };
      }
      case "next": {
        const cursor = state.cursors.get(req.pack);
        const packer = state.packer;
        if (!cursor || !packer)
          return { kind: "error", code: "internal", message: "pack not open" };
        const offset = Math.max(cursor.from, cursor.produced - cursor.buffered);
        while (cursor.buffered < PIECE && cursor.next < cursor.chunks.length) {
          const chunk = cursor.chunks[cursor.next] ?? 0;
          const bytes = await readChunk(state, chunk);
          packer.addChunk(chunk, bytes);
          cursor.next += 1;
          const start = cursor.produced;
          cursor.produced += bytes.length;
          // Only bytes at or after `from` are sent; earlier ones were hashed for the pack hash.
          if (cursor.produced > cursor.from) {
            const skip = Math.max(0, cursor.from - start);
            const part = skip > 0 ? bytes.subarray(skip) : bytes;
            cursor.buffer.push(part);
            cursor.buffered += part.length;
          }
        }
        const all = concat(cursor.buffer, cursor.buffered);
        const size = Math.min(PIECE, all.length);
        const piece = all.slice(0, size);
        const rest = all.subarray(size);
        cursor.buffer = rest.length > 0 ? [rest] : [];
        cursor.buffered = rest.length;
        const last = cursor.next >= cursor.chunks.length && cursor.buffered === 0;
        if (last) state.cursors.delete(req.pack);
        return { kind: "piece", offset, bytes: piece, last };
      }
      case "hash":
        return { kind: "hash", blake3: state.lib.hashHex(req.bytes) };
      case "manifest": {
        const packer = state.packer;
        if (!packer) return { kind: "error", code: "internal", message: "no plan" };
        const bytes = packer.buildManifest(req.identity, req.execution);
        return { kind: "manifest", bytes, blake3: state.lib.hashHex(bytes) };
      }
    }
  } catch (e) {
    if (e instanceof FileChanged)
      return {
        kind: "error",
        code: "file_changed",
        message: `${e.message} changed since you picked the folder. Pick the folder again.`,
      };
    return { kind: "error", code: "internal", message: e instanceof Error ? e.message : String(e) };
  }
}
