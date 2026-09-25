# Agent 5 status

DevOps & Security (`crates/vgames-core`, `crates/vgames-cli`, `xtask`, `.github/**`, `deny.toml`,
`apps/desktop/src-tauri/src/updater/**`, `docs/security/**`, `runtimes/**`, `scripts/release-notes/**`,
`.changes/` tooling).

## Done
- A5-T01 CI foundation — https://github.com/wouhliss/vgames/pull/3 (all jobs green on the first run).
  `ci.yml`, `deny.toml`, `.gitleaks.toml`, required checks and branch-protection settings in `docs/security/ci.md`.
  Gitleaks acceptance: https://github.com/wouhliss/vgames/pull/4 (planted fake `vga_` token → `Secret scan` failed; closed).
- A5-T02 formats (paths, layout, manifest, compat) — pushed to main before CI existed, compat in
  https://github.com/wouhliss/vgames/pull/7.
- A5-T03 signatures, fingerprints, key files, WASM exports — https://github.com/wouhliss/vgames/pull/5.
- A5-T04 trust bundles, `verify_manifest`, `verify_compat_profile` — pushed to main, compat part in PR 7.
- A5-T08 (part 1) changelog lint + PR gate — https://github.com/wouhliss/vgames/pull/9.
- A5-T10 security gates (CODEOWNERS, ownership check, review checklist) — https://github.com/wouhliss/vgames/pull/14.
- A5-T08 release-notes agent + release assembly — https://github.com/wouhliss/vgames/pull/16.
- `contract:` drop fuzzing — https://github.com/wouhliss/vgames/pull/12 (merged).
- A5-T05 as redefined: no-panic property tests (`crates/vgames-core/tests/no_panic.rs`) for manifest, envelope,
  trust bundle, key file and compat parsers (arbitrary bytes + mutated valid documents), plus the existing path and
  manifest properties. **Fuzzing was removed completely at the owner's request** (no fuzz targets, corpora,
  workflows, nightly toolchain or cargo-fuzz).

## In progress
- A5-T09 launcher updater (in review). Then A5-T07 release pipelines, A5-T06 server commands (`login`, `trust publish`,
  `publish`, `trust re-sign`), A5-T12 runtime catalog, A5-T11 adversarial tests, A5-T13 runbooks.

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

- **Trust bundles and `verify_manifest`** (A5-T04, for Agents 1 and 2):
  - `vgames_core::trust::verify_bundle(bytes, &Signature, &RootPin, last_seen_version: Option<u64>, server_id)
    -> Result<VerifiedBundle { state: TrustState, pin: RootPin, rotated, newer }, TrustError>`. Signature is checked
    first over the exact bytes. `newer == false` means same version (a refresh): **the server stores only `newer`
    bundles** and rebuilds `publisher_keys` from `state.bundle().publishers`. Launchers persist `pin` after each
    verification (`RootPin { root, next_root }`): that is how `next_root` rotation re-pins automatically.
  - `TrustState::{key_status(&KeyId) -> Trusted(&PublisherKey) | Revoked | Unknown, is_expired(now), version(),
    expires_at(), bundle()}`; `trust::{TrustBundle, PublisherKey, Revocation, NextRoot, SignedBundle (the
    `GET /v1/trust/bundle` body), sign_bundle}`.
  - `vgames_core::verify::verify_manifest(&TrustState, &Envelope, manifest_bytes, &ExpectedRelease, installed_sequence,
    VerifyMode) -> Result<VerifiedManifest { manifest, digest, key_id, holder_user_id }, VerifyError>` with
    `VerifyMode::Install { now, allow_older }` (launcher install/update: refused while the bundle is expired),
    `VerifyMode::Launch` (pre-launch: only revocation blocks), `VerifyMode::Server { now, caller }` (finalize /
    re-sign: key validity window + `holder_user_id == caller`). One `VerifyError` variant per step.

- **Key files and WASM exports** (A5-T03, for Agents 2 and 3 and my CLI):
  - `vgames_core::keyfile::{KeyFile, KeyKind::{Root, Publisher}, KeyFileError}`: `KeyFile::encrypt(&SecretKey, kind,
    label, created_at, passphrase) -> KeyFile` (fresh random salt + nonce), `KeyFile::parse(bytes)` (no passphrase
    needed: `kind`, `key_id`, `public_key`, `label`, `created_at` are public), `decrypt(passphrase) -> SecretKey`
    (zeroized on drop), `to_bytes()` (write it `0600`). Errors: `WrongPassphrase` (distinct), `AuthenticationFailed`
    (modified header or ciphertext), `UnsupportedKdf` (Argon2id params are fixed at m=64 MiB t=3 p=1).
  - WASM (feature `wasm` of `vgames-core`): `keyfileInfo(bytes)`, `fingerprint(publicKeyBase64)`,
    `UnlockedKey.unlock(bytes, passphrase)` (publisher keys only) → `.keyId`, `.signManifestDigest(blake3Hex)` /
    `.signCompatDigest(blake3Hex)` returning the `vgames.sig/1` envelope JSON, `.free()`. The key never leaves WASM
    memory. **Agent 2:** add `pub use vgames_core::wasm::{KeyfileInfo, UnlockedKey, fingerprint, keyfile_info};` to
    `crates/vgames-pack/src/wasm.rs`; without a reference the linker drops core's exports (I checked: with that line
    they appear in `vgames_pack.d.ts`). **Agent 3:** the admin upload worker uses exactly these; post only the
    envelope JSON to the main thread.

- **Compat profiles** (A5-T02/T04, for Agents 1 and 2): `vgames_core::compat::{parse_and_validate(bytes),
  CompatProfile, Target, Status, RunnerKind, Graphics, Runner, AppliesTo, WINETRICKS_ALLOWLIST, is_launcher_owned_env_key,
  is_valid_dll_name, is_valid_dll_mode}`, `CompatProfile::{applies_to(platform, sequence), wine_dll_overrides()}`, and
  `vgames_core::verify::verify_compat_profile(&TrustState, &Envelope, bytes, &ExpectedCompat { server_id, package_id,
  target }, last_revision, VerifyMode) -> VerifiedCompat { profile, digest, key_id, holder_user_id, newer }`. Same key
  rules as manifests; launchers refuse a lower revision, the server (`VerifyMode::Server`) a lower **or equal** one.

- **Changelog lint for every agent** (A5-T08, part 1): CI job `Changelog fragments` fails a PR that adds no
  `.changes/*.md` or has an invalid one; run `cargo xtask changelog lint` locally. Rules and good/bad examples:
  `AGENTS.md` → "How to write your fragment". `echo "text" | cargo xtask changelog lint-text --type fixed` checks one
  player-facing sentence. All 27 existing fragments pass.

- **`vgames` CLI, offline ceremonies** (A5-T06 part 1, for server owners and admins):
  `vgames keys init-root | issue-publisher | show` and `vgames trust build | sign | verify` (`--help` has examples;
  `--passphrase-env VAR` / `--passphrase-file PATH` for scripts). `trust build` carries every previous revocation
  forward and refuses to re-trust a revoked key; `trust sign` writes the `{bundle, signature}` JSON that
  `POST /v1/admin/trust/bundles` takes. **Agent 1:** use it to make test bundles for A1-T08 (see
  `crates/vgames-cli/tests/ceremony.rs`). Next: `login`, `trust publish`, `publish`, `trust re-sign` (need A1-T08/T11
  and Agent 2's upload library).

- **CI for everyone** (A5-T01): every PR runs `.github/workflows/ci.yml` (Rust fmt/clippy/tests with Postgres 18,
  sqlx offline check + migrations on an empty DB and upgrade from the base revision, WASM + pack-wasm smoke,
  Biome/typecheck/Vitest/OpenAPI lint, launcher Playwright (mock), desktop clippy with WebKitGTK, cargo-deny,
  cargo-audit, pnpm audit, gitleaks). **How to wait for CI with our token:** `gh pr checks` and `gh run watch` fail
  (the token cannot read checks/annotations). Poll instead:
  `id=$(gh run list -R wouhliss/vgames --branch <your-branch> -L 1 --json databaseId --jq '.[0].databaseId')`, then
  `gh run view $id -R wouhliss/vgames --json status,conclusion,jobs` until `status` is `completed`; merge with
  `gh pr merge <n> -R wouhliss/vgames --rebase` only when `conclusion` is `success`.
  PR creation works now (`gh pr create`). Agent 3's request (desktop e2e in CI) and Agent 2's (pack-wasm build +
  smoke) are included.
- Formatting-only fixes I made so `main` is green (please pull before editing): `apps/api/src/storage/mod.rs`,
  `crates/vgames-pack/tests/roundtrip.rs` (rustfmt), `packages/pack-wasm/test/smoke.mjs`, `infra/gcs/cors.json`
  (Biome); `biome.json` migrated (`preset`) and now ignores `.sqlx/`, `**/tests/vectors`, `**/snapshots`, `**/pkg`.

- **Launcher updater** (A5-T09, for Agent 3): commands `updater_status() -> UpdaterStatus`, `updater_check() ->
  UpdaterStatus` (the onboarding "launcher too old" prompt asked for this), `updater_whats_new() -> WhatsNew`,
  `updater_install()`; event `UpdaterStatus { current_version, state: idle | checking | up_to_date | available{version,
  date} | downloading{version, downloaded, total} | installed{version} | failed{message}, blocked: game_running |
  downloads_active | null }`. `WhatsNew { releases: [{version, date, entries: [{type, text}]}], from_latest_notes }`,
  newest first; a release with no entries shows "Stability and performance improvements."; render text as plain text.

## Needs from others
- From Agent 2: regenerate `apps/desktop/src/bindings.ts` after the updater PR (I cannot build the desktop crate on
  this machine; it registers 4 commands + the `UpdaterStatus` event). I added ~20 lines of wiring in your files
  (`lib.rs`: module, plugin, `updater::init`; `commands/mod.rs`; `names.rs`; `capabilities/main.json`; `Cargo.toml`: semver).
- From Agent 2: a way to pause active downloads at their next checkpoint (A2-T04/T08). `updater_install` waits until
  every install reports `InstallPhase::Paused` or finishes; today it just waits.

- **Security gates for every agent** (A5-T10): `.github/CODEOWNERS` (every file owned; security-critical paths in a
  last, checked section), `cargo xtask codeowners check` in CI, and `docs/security/review-checklist.md`, linked from the
  PR template's security section. PRs touching a security-critical path go through that checklist.

- **Release notes** (A5-T08, for the release workflow A5-T07): `scripts/release-notes` (claude-opus-5, structured
  output, guards, deterministic fallback) and `cargo xtask changelog export | release <version>`. Dry run on the real
  repo (no API key → fallback): 35 fragments folded, the 2 user launcher fragments became the player notes.
  The live-API dry run on a test tag needs `ANTHROPIC_API_KEY` in the `release` environment (humans).

## Blockers / contract questions
- Fuzzing is dropped (owner decision 2026-09-25, contract PR 12 merged). **Agents 1 and 2: do not add `cargo fuzz`
  targets**; write `proptest` no-panic properties instead.
- Clarifications I implemented where the docs were silent (a `contract:` PR to 02/09 will record them):
  (1) paths: a component whose NFKC form breaks a rule is refused (`‥`, `／`, fullwidth reserved names) — needed for
  "never escapes after any normalization"; (2) manifests: `files` sorted byte-wise by path (02 §4 step 2), empty files
  hash to BLAKE3(""), `version_label` 1–64 chars; (3) compat env: besides the manifest denylist, keys the launcher sets
  itself are refused (`WINEPREFIX`, `WINEDLLOVERRIDES`, `WINEDLLPATH`, `WINEPATH`, `WINELOADER`, `WINESERVER`,
  `PROTONPATH`, `GAMEID`, `STORE`, `STEAM_COMPAT_*`, `UMU_*`, `PRESSURE_VESSEL_*`), else a profile could redirect the prefix
  or bypass the DLL-override rules; (4) key files fix Argon2id params (a file cannot ask for other costs).
