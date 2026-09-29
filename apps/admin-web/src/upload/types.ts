// Shared shapes for the upload workers (the pack worker plans, hashes and streams pack bytes; the
// key worker holds the decrypted publisher key and only signs digests).

/** A file picked by the admin, as planned: path inside the folder, size, last modified (ms). */
export interface PlannedFile {
  path: string;
  size: number;
  mtime_ms: number;
}

/** The subset of pack-wasm's `WasmPacker` the worker uses (crates/vgames-pack/src/wasm.rs). */
export interface Packer {
  files(): PlannedFile[];
  packCount(): number;
  packSize(pack: number): number | undefined;
  packChunks(pack: number): Uint32Array | number[];
  chunkExtents(chunk: number): { file: number; offset: number | bigint; len: number | bigint }[];
  addChunk(chunk: number, bytes: Uint8Array): void;
  buildManifest(identity: ManifestIdentity, execution: Execution | undefined): Uint8Array;
  free?(): void;
}

export interface PackLib {
  plan(files: PlannedFile[], directories: string[]): Packer;
  /** Lowercase hex BLAKE3 of `bytes`. */
  hashHex(bytes: Uint8Array): string;
}

export interface KeyLib {
  keyfileInfo(bytes: Uint8Array): { kind: string; keyId: string; label: string };
  unlock(
    bytes: Uint8Array,
    passphrase: string,
  ): {
    keyId: string;
    signManifestDigest(hex: string): string;
    signCompatDigest(hex: string): string;
    free(): void;
  };
}

export interface ManifestIdentity {
  server_id: string;
  package_id: string;
  version_id: string;
  sequence: number;
  version_label: string;
  platform: string;
  /** Unix seconds. */
  created_at: number;
}

export interface LaunchTarget {
  id: string;
  label: string;
  executable: string;
  args: string[];
}

export interface Execution {
  launch?: { default: string; targets: LaunchTarget[] };
}

export interface PlanSummary {
  files: PlannedFile[];
  packs: number[];
  totalBytes: number;
}

/** Requests the pack worker answers (one reply per request, matched by id). */
export type PackRequest =
  | { kind: "plan"; files: PlannedFile[]; directories: string[]; blobs: File[] }
  | { kind: "open"; pack: number; from: number }
  | { kind: "next"; pack: number }
  | { kind: "manifest"; identity: ManifestIdentity; execution: Execution | undefined }
  /** BLAKE3 of small documents (compat profiles). */
  | { kind: "hash"; bytes: Uint8Array };

export type PackReply =
  | { kind: "planned"; summary: PlanSummary }
  | { kind: "invalid"; paths: string[] }
  | { kind: "opened" }
  /** Stored bytes of the pack starting at `offset`; `last` when the pack is complete. */
  | { kind: "piece"; offset: number; bytes: Uint8Array; last: boolean }
  | { kind: "manifest"; bytes: Uint8Array; blake3: string }
  | { kind: "hash"; blake3: string }
  | {
      kind: "error";
      code: "file_changed" | "unreadable" | "wasm_missing" | "internal";
      message: string;
    };

export type KeyRequest =
  | { kind: "unlock"; keyfile: Uint8Array; passphrase: string }
  | { kind: "sign"; digest: string; context: "manifest" | "compat" };

export type KeyReply =
  | { kind: "unlocked"; keyId: string; label: string }
  | { kind: "signed"; envelope: string }
  | {
      kind: "error";
      code:
        | "wrong_passphrase"
        | "root_key"
        | "invalid_keyfile"
        | "locked"
        | "wasm_missing"
        | "internal";
      message: string;
    };
