# Agent 5 status

DevOps & Security (`crates/vgames-core`, `crates/vgames-cli`, `xtask`, `.github/**`, `deny.toml`,
`apps/desktop/src-tauri/src/updater/**`, `docs/security/**`, `runtimes/**`, `scripts/release-notes/**`,
`.changes/` tooling).

## Done

## In progress
- A5-T03 (first slice, critical path): `vgames_core::sign` — key ids, `VG1-…` fingerprints, domain-separated
  signatures, `vgames.sig/1` envelopes. Lands first because it unblocks `/.well-known/vgames.json` (Agent 1).
- Next, in order: A5-T01 CI → A5-T02 paths / layout / manifest → A5-T04 trust + `verify_manifest` →
  A5-T03 keyfile + WASM → A5-T02 compat.

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

## Needs from others

## Blockers / contract questions
