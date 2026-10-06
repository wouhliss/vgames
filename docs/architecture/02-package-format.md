# 02 — Package Format, Signing and Transfers

Goals, in priority order:

1. **Integrity:** every byte written into an install is checked against a signed manifest *before* it reaches its file.
2. **No double footprint:** a download writes straight into final files. No archive, no temp copy, no extraction step.
3. **Saturate bandwidth:** many parallel range requests directly to GCS; hashing and writing never become the bottleneck.
4. **Resumable and crash-safe** at chunk granularity, for both upload and download.
5. **Cheap updates:** unchanged files are never re-downloaded.

The single implementation of the format lives in `vgames-core` (types, validation, signature) and
`vgames-pack` (layout planning). `vgames-transfer` implements the I/O.

## 0. Where the bytes live (at a glance)

```
 ADMIN'S FOLDER                    GOOGLE CLOUD STORAGE (server)                 PLAYER'S DISK (after install)
 Game/                             v1/<package>/<version>/                       <library>/half-life-2/
   bin/game.exe      100 MB   ──▶    manifest.json   signed list of every   ──▶    Game/bin/game.exe      100 MB
   data/level1.pak   1.2 GB          manifest.sig    file, chunk and hash          Game/data/level1.pak   1.2 GB
   config.ini          4 KB          packs/00000.pack  ┐ ≤ 256 MiB each            Game/config.ini          4 KB
   readme.txt          2 KB          packs/00001.pack  │ = chunks laid           Game/readme.txt          2 KB
                                     packs/00002.pack  ┘   end to end            .vgames/  (manifest copy +
                                                                                  install record, a few MB)
 inside one pack:
 ┌────────────────────┬──────────────────┬────────────────────┬────────────────────────────────┐
 │ chunk 0 (raw 4 MiB)│ chunk 1 (zstd,   │ chunk 2 (raw 4 MiB)│ chunk 25 (raw 6 KB):           │
 │ game.exe bytes 0–4M│ 3.1 MiB on the   │ game.exe 8M–12M    │ config.ini + readme.txt        │
 │                    │ wire → 4 MiB)    │                    │ (small files share a chunk)    │
 └────────────────────┴──────────────────┴────────────────────┴────────────────────────────────┘
```

- **On the player's disk, files are stored exactly as the admin uploaded them:** the same folders and
  names, plain uncompressed bytes, directly playable. There is no archive, no container format, and no
  compressed-at-rest storage. The only addition is a small `.vgames/` folder with the signed manifest
  and the install record.
- **On the server, packages are stored as packs**, which are plain concatenations of chunks.
  Packs exist for transfer speed: 100,000 small files as 100,000 objects would mean 100,000
  requests, while packs let every connection stream large contiguous ranges.
- **Compression is per chunk, on the server side only, and optional.** A chunk is stored zstd-compressed
  only if that saves at least 10% (text, uncompressed assets); already-compressed data (most game
  archives, video, audio) stays raw. The launcher decompresses each chunk **in memory** as it
  arrives and writes plain bytes into the final file. There is never a compressed copy on disk and never
  a second extraction pass. Packing with `compression = none` stores every chunk raw; the downloader
  handles both.

## 1. Vocabulary

| Term | Definition |
|---|---|
| **File** | A regular file in the package tree. No symlinks, hardlinks, devices or ACLs. |
| **Chunk** | ≤ 4 MiB (4,194,304 bytes) of decoded file data: part of one large file, or several whole small files. The unit of hashing and verification. |
| **Encoding** | How a chunk is stored in a pack: `raw`, or `zstd` (one independent zstd frame, decoded in memory). Never a whole-archive compression. |
| **Pack** | A GCS object ≤ 256 MiB: the stored bytes of consecutive chunks, concatenated. The unit of upload and of signed URLs. |
| **Manifest** | Signed JSON describing files, chunks, packs, launch targets, saves, controllers. |

## 2. Storage layout

```
packages bucket   v1/{package_id}/{version_id}/manifest.json
                  v1/{package_id}/{version_id}/manifest.sig
                  v1/{package_id}/{version_id}/packs/{pack_index:05}.pack
assets bucket     v1/{package_id}/{asset_id}.{jpg|png|webp}
saves bucket      v1/{user_id}/{blake3_hex}
```

The server tracks versions that are aborted or failed. A cleanup job deletes their
objects after 7 days. Buckets are private: all access is through signed URLs.

## 3. Path rules (`vgames-core::paths`)

A manifest path is valid only if **all** of these hold. The packer refuses to pack
an invalid tree, and the server and launcher refuse invalid manifests:

- UTF-8, already in Unicode **NFC** (rejected, never silently normalized).
- Relative, `/`-separated; no leading `/`, no `\`, no empty, `.` or `..` components.
- ≤ 512 bytes total, ≤ 255 bytes per component, ≤ 64 components.
- No control characters (U+0000–U+001F, U+007F) and none of `< > : " | ? *`.
- No component ends with `.` or space.
- No component is a Windows reserved name, case-insensitive, with or without
  extension: `CON PRN AUX NUL COM0-9 LPT0-9 CONIN$ CONOUT$`, plus the superscript digit forms `COM¹²³ LPT¹²³`.
- Each component's **NFKC compatibility form** must also obey the structure, forbidden-character,
  trailing-dot/space and reserved-device-name rules; the first component's compatibility form must
  not be `.vgames`. Validate that form for safety without rewriting the original NFC path
  (`vgames_core::paths::validate_path`, test `paths::tests::rule_compatibility_forms`).
- Unique under Unicode simple case folding (portable to NTFS/APFS).
- No path is a prefix directory of a file path that is also listed as a file.
- The first component is not `.vgames` (reserved for launcher metadata).

The launcher additionally canonicalizes each target path and checks it stays
under the install root (defense in depth against junctions/symlinks planted in
the install dir), and uses `\\?\` extended-length paths on Windows.

## 4. Deterministic layout (`vgames-pack`)

Input: a folder. Options: `chunk_size = 4 MiB` (fixed in v1), `pack_size = 256 MiB`,
`compression = none | auto`.

1. Walk the tree; reject symlinks/special files; validate every path (§3).
2. Sort files by path, byte-wise on UTF-8.
3. Assign chunks, iterating files in order:
   - **Empty file:** no chunk (`chunk` is `null`).
   - **Large file** (`size ≥ chunk_size`): close the open shared chunk, if any. The file gets
     `ceil(size / chunk_size)` **exclusive** chunks; all are full except the last.
   - **Small file:** if the open shared chunk cannot fit it, close that chunk. Append the file
     to the open shared chunk at `offset = bytes already in the chunk`.
   - Close the open shared chunk at the end.

   Every file maps to either one range inside one chunk (small) or a run of whole chunks
   (large). Extents are **implied** by `(chunk, offset, size)`. Unchanged large files
   keep identical chunk hashes across versions.
4. Encode each chunk: `raw`, or, with `compression = auto`, `zstd` level 3 if the
   frame is ≥ 10% smaller than raw. The browser packer is always `raw`.
5. Pack chunks in order: append stored bytes to the current pack; start a new pack
   when the next chunk would push it over `pack_size`.
6. Hash in the same single read pass: chunk BLAKE3 (decoded bytes), file BLAKE3,
   pack BLAKE3 (stored bytes).

**Upload streams packs straight from the source files.** Pack bytes are generated on
the fly from the plan and never written to disk. A resumed upload regenerates bytes
from any offset; zstd output is deterministic for a given input and level.
If any source file's size or mtime changes during upload, the upload aborts.

## 5. Manifest (`vgames.manifest/1`)

Exact UTF-8 JSON bytes, ≤ 256 MiB, stored as `manifest.json`. Signed as described in 01-security §3.4.
**Execution-relevant data lives only here**: launch targets, arguments, environment,
save locations, controller support, join arguments. Display metadata (title,
description, images) lives in the API and is never used for execution.

```json
{
  "format": "vgames.manifest/1",
  "server_id": "01920000-0000-7000-8000-000000000000",
  "package_id": "0192a6f0-1c2d-7e3f-8a9b-0c1d2e3f4a5b",
  "version_id": "0192a6f1-aaaa-7bbb-8ccc-dddddddddddd",
  "sequence": 12,
  "version_label": "1.4.2",
  "platform": "windows-x86_64",
  "created_at": "2026-09-24T10:00:00Z",
  "chunk_size": 4194304,
  "totals": { "files": 3, "bytes": 104865792, "chunks": 26, "packs": 1 },

  "packs": [
    { "size": 98566144, "blake3": "…64 hex…" }
  ],
  "chunks": [
    { "pack": 0, "offset": 0,       "stored_size": 3102114, "size": 4194304, "encoding": "zstd", "blake3": "…" },
    { "pack": 0, "offset": 3102114, "stored_size": 4194304, "size": 4194304, "encoding": "raw",  "blake3": "…" }
  ],
  "files": [
    { "path": "Game/Binaries/Win64/Game.exe", "size": 104857600, "blake3": "…", "executable": true, "chunk": 0, "offset": 0 },
    { "path": "Game/config.ini", "size": 4096, "blake3": "…", "executable": false, "chunk": 25, "offset": 0 },
    { "path": "Game/readme.txt", "size": 4096, "blake3": "…", "executable": false, "chunk": 25, "offset": 4096 },
    { "path": "Game/empty.flag", "size": 0, "blake3": "af1349b9…", "executable": false, "chunk": null, "offset": 0 }
  ],
  "directories": ["Game/Saved"],

  "launch": {
    "default": "play",
    "targets": [
      {
        "id": "play",
        "label": "Play",
        "executable": "Game/Binaries/Win64/Game.exe",
        "args": ["-dx12"],
        "working_dir": "Game/Binaries/Win64",
        "env": { "GAME_LANG": "en" }
      }
    ]
  },
  "controllers": { "supported": ["xinput"], "emulate_as": "xbox360" },
  "saves": {
    "locations": [
      { "id": "main", "base": "documents", "path": "My Games/Example/Saves", "include": ["**/*"], "exclude": ["**/*.log"] }
    ]
  },
  "multiplayer": {
    "join": { "target": "play", "args": ["+connect", "{join_secret}"] }
  }
}
```

Validation (`vgames_core::manifest::parse_and_validate` and `Manifest::validate`, run by server and launcher):

- `format` known; UUIDs valid; `sequence > 0`; `platform` in the allowed list; `chunk_size == 4194304`.
- `version_label`: 1–64 Unicode scalar values, with no control character
  (`vgames_core::manifest`, test `manifest::tests::rule_version_label`).
- Duplicate JSON object keys are refused, including string-map fields such as environment variables;
  do not interpret a last duplicate as authoritative (`vgames_core::manifest::parse_and_validate`,
  test `manifest::tests::rejects_duplicate_json_keys`).
- Input is bounded to 256 MiB. Counts are bounded: at most 64 launch targets, 64 save locations,
  256 combined include/exclude patterns per save location, and 256 environment variables per target.
  Arguments total at most 64 KiB; each environment value is at most 32 KiB. Enforced by
  `vgames_core::manifest`; tests `xtask/tests/package_format.rs::bounded_counts`,
  `manifest::tests::rule_launch_args_limit`, `manifest::tests::rule_env_keys_and_values` and
  `manifest::tests::rejects_oversized_and_malformed_json`.
- Chunks: contiguous within each pack in order (`offset` = previous `offset + stored_size`),
  `size ≤ chunk_size`, `stored_size ≤ size + 64 KiB` (zstd worst case), `raw ⇒ stored_size == size`.
- Packs: `size == Σ stored_size` of their chunks, `≤ 256 MiB`.
- Files must be strictly ordered by their original UTF-8 path bytes, without locale-aware sorting
  (`vgames_core::manifest`, test `manifest::tests::rule_files_sorted`).
- An empty file has no chunk, zero offset and the BLAKE3 of zero bytes:
  `af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262`
  (`vgames_core::manifest::EMPTY_BLAKE3`, test `manifest::tests::rule_empty_files`).
- Files: layout exactly matches §4 (recomputed from sizes in order). Every chunk byte is
  covered exactly once. `Σ size == totals.bytes`, counts match `totals`.
- Paths valid (§3); `directories` valid and not files.
- `launch.targets[*].executable` refers to a listed file; `working_dir` is a listed
  directory prefix; `args` ≤ 64 × 1024 bytes; `env` keys `^[A-Z_][A-Z0-9_]{0,63}$` and not
  in a denylist (`PATH`, `LD_PRELOAD`, `LD_LIBRARY_PATH`, `DYLD_*`, `PYTHONPATH`, `COMSPEC`).
- `saves.locations[*].base` ∈ {`install`, `home`, `documents`, `saved_games`, `appdata`,
  `localappdata`, `xdg_data`, `xdg_config`}; `path` passes §3 rules.
- `controllers.supported` ⊆ {`xinput`, `dualshock4`, `dualsense`, `switch_pro`, `generic`};
  `emulate_as` ∈ {`xbox360`, `dualshock4`} or absent.
- `multiplayer.join.args` may contain the placeholder `{join_secret}` only as a whole argument.

## 6. Publishing (upload) protocol

```
Packer (CLI / launcher admin mode / browser worker)                     API                        GCS
 1  scan + plan (§4)
 2  POST /v1/admin/packages/{pid}/versions {platform, version_label} ─▶ row 'uploading'
    ◀── {version_id, sequence, server_id}
 3  for each pack i (4–16 in parallel, adaptive):
      POST /v1/admin/versions/{vid}/packs/{i}/upload-session ────────▶ signed resumable-start URL
      POST <url> (x-goog-resumable: start) ─────────────────────────────────────────────────────▶ session URI
      PUT session URI, 16 MiB pieces, Content-Range ─────────────────────────────────────────────▶ (resume on failure:
                                                                                               PUT bytes */* → offset)
 4  build manifest (ids + sequence from step 2) → sign locally (publisher key)
 5  POST /v1/admin/versions/{vid}/manifest-upload ───────────────────▶ signed PUT URL
    PUT manifest.json ──────────────────────────────────────────────────────────────────────────▶
 6  POST /v1/admin/versions/{vid}/finalize {manifest_blake3, manifest_size, signature} ─▶
        server: fetch manifest, check size+hash, verify signature (key holder == caller,
        key valid now, not revoked), validate manifest, check ids/sequence/platform,
        check every pack object exists with the manifest size; store manifest.sig;
        state → 'verifying'; enqueue job version.verify
    ◀── 202 {state: 'verifying'}
 7  job version.verify: stream every pack from GCS, decode + hash every chunk,
        check pack hashes → 'ready' | 'failed' (with first failing pack/chunk)
 8  POST /v1/admin/versions/{vid}/publish ───────────────────────────▶ package_releases → this version
```

- Upload sessions have a 15-minute *start* window, and GCS keeps a session resumable for 7 days.
  The packer persists `{version_id, pack → session URI, confirmed offset}` in a local resume file.
- Abort: `DELETE /v1/admin/versions/{vid}` (only if not published) → state `aborted`, objects deleted by job.
- A published version is never modified. A bad version is **yanked** (`POST …/yank`).
  Launchers on a yanked version are offered the current release even if its
  sequence is lower. That is the only allowed automatic downgrade prompt, and the user confirms it.

## 7. Download / install algorithm (`vgames-transfer::download`)

```
 descriptor ──▶ manifest (stream, BLAKE3 must match) ──▶ VERIFY SIGNATURE (01 §3.4) ──▶ validate
      │
      ▼
 choose library → target dir → free-space check → .vgames/install.json {state: installing}
      │
      ▼
 preallocate every file at final size ──▶ plan ranges over missing chunks
      │
      ▼
 ┌─ N network workers (adaptive 2..32) ───────────┐     bounded channel      ┌─ M writer threads ─────────┐
 │ GET pack URL, Range: whole chunks, ≤ 32 MiB    │  (chunk_id, buffer) ───▶ │ positional writes into     │
 │ stream → per-chunk buffer (pooled)             │                          │ final files (LRU handles)  │
 │ BLAKE3 verify (decode zstd first if needed)    │                          │ mark chunk done in bitset  │
 └────────────────────────────────────────────────┘                          └─────────────┬──────────────┘
                                                            every 2 s: fsync dirty files → journal (atomic)
      ▼
 all chunks done → fsync → exec bits → empty dirs → install.json {state: installed} (atomic) → notify UI
```

Requirements:

1. **Signature first.** No file is created before the manifest verifies.
2. **Free space:** `required = total bytes − reusable bytes + 64 MiB`. Refuse with a clear
   message showing required vs available. Refuse on filesystems whose max file size is
   too small (FAT32: 4 GiB − 1).
3. **Preallocate** each file to its final size (`set_len`, plus `fallocate` on Linux and
   `FileAllocationInfo` on Windows where supported), so disk-full surfaces at the
   start and files are not fragmented. Footprint = final size. Nothing else.
4. **Ranges** cover whole chunks of one pack, ≤ 32 MiB, ordered by pack then offset.
   Expect `206` with an exact `Content-Range`; anything else is an error.
5. **Verification before write:** a chunk's buffer goes to the writer only after
   its BLAKE3 (of decoded bytes) matches the manifest. zstd decoding is bounded to
   `size` bytes (decompression-bomb safe).
6. **Concurrency control (AIMD):** start at 6 connections; every 2 s add 2 while
   aggregate throughput rose ≥ 5%, up to 32; halve on timeouts/resets/5xx (min 2).
   Use HTTP/1.1 connections (one TCP congestion window each). Do not multiplex over HTTP/2.
7. **Memory bound:** buffer pool of at most 48 × 4 MiB. Workers block on the pool, and writers
   return buffers. The steady state allocates nothing.
8. **Writers:** a dedicated blocking pool (4 threads). `pwrite`/`seek_write` at
   `file offset = (chunk − file.chunk) × chunk_size + …` per the implied extents. Keep an
   LRU of ≤ 128 open handles.
9. **Journal:** `.vgames/journal.bin`: header (version_id, manifest BLAKE3) + chunk bitset.
   Persisted every 2 s: fsync the files written since the last persist, then write the journal
   to a temp file, fsync, rename. Resume = load bitset, plan the missing chunks.
10. **Errors:**

| Condition | Action |
|---|---|
| `403` / expired URL | Refresh signed URLs for that pack (`POST /v1/versions/{vid}/download-urls`), retry, no backoff penalty |
| `5xx`, reset, timeout, 20 s without bytes | Retry the remaining chunks of the range; exponential backoff 0.5 s → 30 s with full jitter; AIMD decrease |
| Chunk hash mismatch | Retry that chunk once on a fresh connection; second mismatch → `POST /v1/versions/{vid}/integrity-reports`, fail the install with "the server has a damaged file" |
| Unexpected status / `Content-Range` / length | Treat as a mismatch (above) |
| `ENOSPC` / disk full | Pause the install, notify the user, and resume when space is available |
| Other I/O error | Fail with the OS error and the path; keep the journal so the user can resume |
| Cancel | Stop workers; ask whether to keep the partial install for later or delete it |

11. **Progress** to the UI ≤ 4 Hz: bytes done/total, rate (EWMA 5 s), ETA, active connections, phase.
12. **Throttle** option (token bucket, user setting). Default unlimited.

## 8. Update algorithm (`vgames-transfer::update`)

1. Verify the new manifest (§7.1). Refuse `sequence ≤ installed` unless the user
   explicitly chose a version, or the installed version is yanked.
2. Diff by path: **same** (size, BLAKE3, executable equal) → keep; **removed** → delete at commit;
   **added/changed** → build.
3. For each needed chunk: if the old manifest has a chunk with the same BLAKE3 and
   size, read it from the old install (old files stay intact until commit), verify it, and reuse it.
   Otherwise download it.
4. **Safe mode (default):** changed/added files are built in `.vgames/staging/`
   (same filesystem), then committed. Extra space = size of changed/added files only.
5. **In-place mode** (offered only when free space is short): delete changed files
   first, then write new ones in place. The package cannot be played until the update finishes;
   the UI must say so before starting.
6. **Commit:** write `.vgames/commit.json` (renames and deletions) and fsync it; apply it idempotently;
   write the new `install.json`; delete `commit.json`. On startup, replay any leftover `commit.json`.

## 9. Verify, repair, move, uninstall

- **Verify:** re-hash every file against the manifest (parallel, rate-limited to keep the
  system responsive). Mismatched or missing files → **repair** = update with those files marked changed.
- **Move:** same filesystem → rename. Otherwise copy with hashing against the manifest, then
  delete the source only after the copy verifies.
- **Uninstall:** delete manifest-listed files and `.vgames/` without following
  symlinks. If other files remain (user mods, configs, local saves), list them and
  ask; the default is to keep them.

## 10. Local install layout

```
<library root>/
  .vgames-library.json          {"format":"vgames.library/1","library_id":"…"} (detects moved/removed drives)
  <slug>/                       (or <slug>-2 … if taken by something else)
    …package files…
    .vgames/
      install.json              server_id, package_id, version_id, sequence, state, installed_at
      manifest.json             exact signed bytes
      manifest.sig
      journal.bin               during install/repair
      staging/  commit.json     during updates
```

`install.json` is written last (temp file, fsync, rename, fsync dir). An install
without `state: installed` is shown as "Incomplete: resume or remove".

## 11. Pre-launch checks

Before every launch (target: < 300 ms for a typical package; cache results keyed by file mtime+size so repeat launches cost < 20 ms):

1. `install.json` state is `installed`.
2. `BLAKE3(manifest.json)` equals the envelope; signature valid; key not revoked in
   the **currently cached** trust bundle (01 §3.2). A revoked key blocks the launch with
   "Re-verify", which fetches a re-signed envelope and runs **Verify** (§9).
3. The launch target executable's BLAKE3 matches the manifest (hash at ≥ 1 GB/s; a 200 MB exe ≈ 0.2 s).
