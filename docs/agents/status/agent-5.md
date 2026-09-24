# Agent 5 status

DevOps & Security (`crates/vgames-core`, `crates/vgames-cli`, `xtask`, `.github/**`, `deny.toml`,
`apps/desktop/src-tauri/src/updater/**`, `docs/security/**`, `runtimes/**`, `scripts/release-notes/**`,
`.changes/` tooling).

## Done

## In progress
- Next, in order: A5-T01 CI → A5-T04 trust + `verify_manifest` → A5-T03 keyfile + WASM → A5-T02 compat.

## Interfaces delivered (other agents may now rely on these)
- **Signatures, key ids, fingerprints** (A5-T03 slice 1, for Agents 1, 2, 3):
  - `vgames_core::PublicKey::{from_bytes, from_base64, to_base64, key_id() -> KeyId, fingerprint() -> Fingerprint}`
    (rejects off-curve and small-order keys). `Fingerprint` displays as `VG1-XXXX-…` (8 groups, matches the
    OpenAPI pattern) and parses the canonical form only. **Agent 1:** `/.well-known/vgames.json` can use
    `PublicKey::from_bytes(&root)?.fingerprint().to_string()` now.
  - `vgames_core::sign::{Context::{Manifest, Compat, Trust}, signing_message(ctx, &Digest)}`,
    `PublicKey::verify(ctx, payload_bytes, &Signature)` / `verify_digest(ctx, &Digest, &Signature)` (strict),
    `SecretKey::{generate, from_seed, sign, sign_digest, public_key}` (zeroized on drop).
  - `vgames_core::Envelope` (`vgames.sig/1`): `Envelope::parse(bytes)` (strict, ≤ 4 KiB), serde
    (`Deserialize` applies the same rules, for `ReleaseDescriptor.signature`), `to_bytes()`,
    `sign(&SecretKey, ctx, payload)`, `verify(&PublicKey, expected_ctx, payload)` and `verify_digest(…)`.
  - `vgames_core::{Digest, Timestamp}`: BLAKE3 digest (lowercase hex, constant-time `==`, `Digest::of(bytes)`),
    RFC 3339 UTC timestamp. `vgames_core::codec::{decode_hex, decode_base64_exact, encode_base64}`.

- **Paths and layout** (A5-T02 slice 1, for Agent 2: drop-in replacements for `vgames_pack::{paths, layout}`):
  - `vgames_core::paths::{validate_path, validate_tree(files, dirs), PathError, fold, MAX_*}`: same names and
    variants as the interim module, plus `PathError::UnsafeCompatibilityForm` (NFKC of a component must also be
    safe: `‥`, `／`, fullwidth `ｃｏｎ`, trailing NBSP…), exact Unicode 18 **simple case folding** (generated table),
    and directory prefixes must be spelled the same way everywhere (`Game/a` + `game/b` is a `CaseCollision`).
  - `vgames_core::layout::{assign_chunks, assign_chunks_bounded, count_chunks, packs (= assign_packs), Layout,
    FileSlot, ChunkSlot, PackSlot, ChunkPlacement, Layout::extents, CHUNK_SIZE, PACK_SIZE, MAX_STORED_OVERHEAD}`:
    your interim API unchanged; `packs` also refuses zero-byte stored chunks.

- **Manifest** (A5-T02, for Agents 1 and 2): `vgames_core::manifest::{parse_and_validate(bytes) -> Result<Manifest,
  ManifestError>, Manifest, Platform, Encoding, Chunk, Pack, File, Launch, LaunchTarget, SaveLocation, SaveBase,
  Controllers, ControllerKind, EmulatedController, Multiplayer, Join, EMPTY_BLAKE3, JOIN_SECRET_PLACEHOLDER,
  is_valid_env_key, is_denied_env_key}`. Every 02 §5 rule, one error variant per rule with the array index
  (`ManifestError::{Chunk, Pack, File, LaunchTarget, Env, Save, …}`). Accepts `vgames-pack`'s exact output
  (its snapshot is a test vector here). Rules the spec implied and I made explicit: files must be sorted byte-wise
  (02 §4 step 2); empty files carry the BLAKE3 of zero bytes; UUIDs lowercase hyphenated, non-nil;
  `version_label` 1–64 chars (as the API); duplicate JSON keys are refused; args/env values without NUL; bounded
  counts (64 targets, 256 env vars, 64 save locations). Verify the signature over the exact bytes **before**
  calling this (A5-T04 `verify_manifest` does both).

## Needs from others

## Blockers / contract questions
