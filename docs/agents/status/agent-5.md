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
- A5-T09 launcher updater — https://github.com/wouhliss/vgames/pull/22 (supersedes PR 17): tested against a local
  update server with throwaway minisign keys; `requireSignedVersion` on.

- A5-T06 part 2: `vgames login | logout`, `trust publish`, `trust re-sign` — https://github.com/wouhliss/vgames/pull/27.
- A5-T06 part 3: `vgames publish` (pack, upload, sign, finalize, `--publish`; resumable; checks the key against the
  server's verified trust bundle before uploading) on Agent 2's `vgames_transfer::upload::publish` —
  https://github.com/wouhliss/vgames/pull/65.
- A5-T06 acceptance: `vgames verify` (a release checked as launchers do) and `scripts/e2e/key-pipeline.sh` extended:
  root init → publisher keys → bundle v1 → publish with the first key (verified) → bundle v2 revokes it (launchers
  refuse) → v1 replay refused → re-sign with the new key → launchers accept it under v2. Passed locally twice
  against the real API (Postgres 18, fs storage, fake Discord); runs nightly in `e2e.yml`. **A5-T06 is complete.**

- A5-T07 part 1: `release-desktop.yml`, `desktop-matrix.yml`, `cargo xtask updater sign | manifest | verify`, actionlint
  in CI, `docs/security/release.md` — https://github.com/wouhliss/vgames/pull/30.
- A5-T07 part 2: `apps/api/Dockerfile` (distroless, non-root, `--read-only`), `release-api.yml` (multi-arch, cosign
  keyless, SBOM + provenance), nightly `e2e.yml` (CLI key pipeline + admin Playwright against the real API), `soak.yml`,
  Dependabot — https://github.com/wouhliss/vgames/pull/33. The fork dry run (acceptance) needs humans.
- A5-T12 part 1: `vgames_core::runtimes` (`verify_catalog`), runtime-catalog test vectors, `cargo xtask runtimes
  build | sign | verify`, `runtimes/catalog.toml` — https://github.com/wouhliss/vgames/pull/35.
- Launcher memory test (Agent 3's `apps/desktop/e2e/memory.spec.ts`, changed with the maintainer's approval): V8 grows
  the heap in steps, so one 100-round window measured 27–308 KB on an idle app and the required check flipped. The
  256 KiB budget now applies to the smallest of three windows (a leak grows every window: 3 KB per round injected
  fails), with listeners and DOM nodes checked after each — https://github.com/wouhliss/vgames/pull/94.
- A5-T11 security test matrix (`docs/security/test-matrix.md`), `cargo xtask security check` (launcher CSP and
  capabilities, signing secrets only in `release`), nightly CI and the admin mock e2e job —
  https://github.com/wouhliss/vgames/pull/95; the fs backend refuses expired storage links (nightly E2E) —
  https://github.com/wouhliss/vgames/pull/97. **A5-T11 is complete**; the matrix's one open row is Agent 2's
  cross-server token test (below).
- A5-T13 part 1: `docs/security/runbooks.md` and `vgames trust publish --new-identity` —
  https://github.com/wouhliss/vgames/pull/98. Part 2: the final security review,
  `docs/security/review-2026-09-30.md` (findings G1, F1–F6 below).

## In progress
- **CI runs on GitHub again (the repository is public since 2026-09-26; Actions minutes are free).** Nothing changes
  in the workflow: open the PR, wait for the required checks, merge. `scripts/ci/local.sh` runs the same jobs locally
  before you push (optional, but it saves a round trip). The manual-only note of 2026-09-26 morning is withdrawn.
- A5-T12 part 2 (runtime watcher `runtimes.yml`, `release-runtimes.yml`, `cargo xtask runtimes upsert`, `tar`
  archives, smoke tests): https://github.com/wouhliss/vgames/pull/46, green, waits for the maintainer's
  runtime-catalog key (`runtimes/runtime-catalog.pub` on main, secrets in `release`).
- A5-T11: security test matrix and gates (https://github.com/wouhliss/vgames/pull/95), then the fs backend's
  expired-link e2e test.
- A5-T13: `docs/security/runbooks.md` (keys, revocation drill with timing, root rotation, updater and catalog key
  compromise, antivirus false positives, compromised server or admin account, communication templates), then the
  final security review.

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
  `crates/vgames-cli/tests/ceremony.rs`).
- **`vgames` CLI, server commands** (A5-T06 part 2, for server owners and admins):
  - `vgames login --server URL` (desktop PKCE flow, paste the code or the whole `vgames://auth/callback` link; the root
    fingerprint is pinned at login, `--fingerprint VG1-…` non-interactively; `--authorize-with PROGRAM` replaces the
    browser in scripts). Tokens in the OS keychain (service `vgames-cli`), else a `0600` file with a warning; refresh
    rotation is persisted before use. `VGAMES_ACCESS_TOKEN=vga_…` for one-shot scripts. `vgames logout` revokes.
  - `vgames trust publish --signed bundle.signed.json`: verified locally (pinned root, server id, newer than the
    server's) before `POST /v1/admin/trust/bundles`.
  - `vgames trust re-sign --from <old key id> --key new.vgkey [--previous old.signed.json] [--finalized-before T]
    [--dry-run]`: lists every `verifying | ready | published` version signed by the old key, verifies each old envelope
    under the old key (so only digests the old key really signed are re-attested), signs the same digest with the new
    key and posts `…/signature`. Checks the new key is trusted in the server's verified bundle first.
  - Smoke-tested against the real API (fake Discord, fs storage): login, bundle v1, bundle v2 revoking the key,
    replay refused, re-sign dry run over the admin listings.

- **Release pipeline** (A5-T07 part 1, for everyone): a `desktop-v*` tag builds a **draft** release in the `release`
  environment (human approval): OS-signed installers, updater artifacts signed for exactly that version
  (`cargo xtask updater sign`; the launcher's `requireSignedVersion` refuses anything else), `latest.json`
  (`cargo xtask updater manifest`, every artifact verified), the agent-written `changelog-user.json`, SBOMs, checksums
  and provenance. Procedure, secret names and the fork dry run: `docs/security/release.md`.
  - **Agents 2 and 4 (overlay):** the release job stages the Authenticode-signed overlay binaries in
    `apps/desktop/src-tauri/resources/overlay/` (`vgames_overlay64.dll`, `vgames_overlay32.dll`, `vgames-inject32.exe`
    when that bin exists, `libvgames_overlay.so`, `crates/vgames-overlay/layers/*.json`). Add them to
    `tauri.conf.json` `bundle.resources` when the overlay ships (Agent 2 owns the config).
  - `desktop-matrix.yml` runs the launcher crates' tests on Windows and macOS too (PRs touching them + nightly):
    **Agents 2 and 4**, expect OS-specific failures there to be reported as real bugs.
- **CI for everyone** (A5-T01): every PR runs `.github/workflows/ci.yml` (Rust fmt/clippy/tests with Postgres 18,
  sqlx offline data + migrations, changelog fragments + code owners, WASM + pack-wasm smoke, Biome/typecheck/Vitest/OpenAPI
  lint, launcher Playwright (mock), desktop clippy with WebKitGTK, cargo-deny/cargo-audit/pnpm audit, gitleaks,
  actionlint). `scripts/ci/local.sh` runs the same jobs on your machine (`--list`, `--help`).
- Formatting-only fixes I made so `main` is green (please pull before editing): `apps/api/src/storage/mod.rs`,
  `crates/vgames-pack/tests/roundtrip.rs` (rustfmt), `packages/pack-wasm/test/smoke.mjs`, `infra/gcs/cors.json`
  (Biome); `biome.json` migrated (`preset`) and now ignores `.sqlx/`, `**/tests/vectors`, `**/snapshots`, `**/pkg`.

- **Runtime catalog** (A5-T12, for Agent 2's runtime manager A2-T16/T17):
  - `vgames_core::runtimes::verify_catalog(bytes, minisig_text, public_key, last_seen_version) -> Result<VerifiedCatalog
    { catalog, newer }, RuntimesError>`: minisign signature over the exact bytes first (prehashed only), then the
    `vgames.runtimes/1` rules, then rollback (`RuntimesError::Rollback { got, seen }`; equal version = refresh,
    `newer == false`). Persist the highest accepted version. `public_key` is the `.pub` file or its base64 line.
  - `Catalog { format, version, generated_at, commercial, runtimes: Vec<Runtime> }`, `Runtime { id: RuntimeId, version,
    os, arch, url, sha256, size, archive, license, min_launcher_version, rosetta_required, macos_max_supported,
    redistribution }`. Every `url` is a plain `https://github.com/<owner>/<repo>/releases/download/<tag>/<file>`; check
    `size` while downloading and `sha256` **before** extracting (09 §5).
  - Test vectors: `crates/vgames-core/tests/vectors/runtimes/` (valid v2, older v1 = rollback, tampered catalog, wrong
    key, the pinned archive and a tampered copy; README lists the expected outcome of each). Use them in your tests.
  - The real public key will be `runtimes/runtime-catalog.pub` (created by humans, `docs/security/release.md`); compile it
    in at build time and keep the runtime manager off in builds without it. Publication: the `runtimes` GitHub release
    (`runtimes.json` + `runtimes.json.minisig`), from `release-runtimes.yml` (next PR).

- **Launcher updater** (A5-T09, for Agent 3): commands `updater_status() -> UpdaterStatus`, `updater_check() ->
  UpdateCheck` (exactly the `up_to_date | available{version} | failed{detail}` shape the onboarding "launcher too old"
  prompt uses; I deleted the delivered stub from `src/ipc/contract.ts`), `updater_whats_new() -> WhatsNew`,
  `updater_install()`; event `UpdaterStatus { current_version, state: idle | checking | up_to_date | available{version,
  date} | downloading{version, downloaded, total} | installed{version} | failed{message}, blocked: game_running |
  downloads_active | null }`. `WhatsNew { releases: [{version, date, entries: [{type, text}]}], from_latest_notes }`,
  newest first; a release with no entries shows "Stability and performance improvements."; render text as plain text.
  `bindings.ts` is regenerated.
- **`AppState::ui_ready` is now a `CancellationToken` latch** (was `Arc<Notify>`), for Agent 2: `app_ready` calls
  `ui_ready.cancel()`, waiters use `ui_ready.cancelled().await`. With `Notify::notify_one` only one of the two waiters
  (window-show fallback, updater scheduler) woke up, so the first update check never ran.

## Needs from others
- **From Agent 2 (A5-T13 review gate G1, before any release):** the UI calls `install_start`, `install_update`,
  `install_verify` and `downloads_*`, but no Rust command implements them yet (A2-T08). When they land, every
  install and update must go through `vgames_transfer::install::fetch_release`/`install` (`verify_manifest` on
  the exact bytes before anything is written), never from the release descriptor alone; I will re-run review items
  1.1, 1.2 and 2.3 on that code (`docs/security/review-2026-09-30.md`).
- **From Agent 2 (A5-T13 finding F1, revocation):** after a publisher key is revoked and its releases re-signed,
  a game installed from such a release is blocked at launch with "Verify" (`LaunchError::KeyRevoked`), and 02 §9 and
  §11 say Verify fetches the re-signed envelope of the installed version. The UI calls `install_verify`, but no
  Rust command implements it yet, so today the player has to reinstall. Please implement it with a test: install
  → revoke → re-sign → Verify replaces `.vgames/manifest.sig` (same manifest bytes, envelope verified under the
  current bundle) → the launch works, without downloading the game again. `docs/security/runbooks.md` §3 states the
  gap until then.
- **From Agent 3 (CI flake, required check "Launcher UI end-to-end"):** `e2e/browse.spec.ts › has no serious
  accessibility violations` failed on https://github.com/wouhliss/vgames/pull/99 with axe reporting 4.41 and 4.45 contrast on the
  screenshot lightbox's "1 of 4" caption, on UI code that passed the same test hours earlier. The dialog backdrop
  fades in over 150 ms and the test scans right after the dialog is visible, so axe sometimes measures mid-fade.
  Proposed: `use: { reducedMotion: "reduce" }` in `apps/desktop/playwright.config.ts` (`tokens.css` already sets
  every motion duration to 0 ms under `prefers-reduced-motion`), so every scan sees the final colours.
- **From Agent 1 (A5-T13 finding F3, low):** `vgames_proto::auth::{TokenRequest, TokenResponse}` derive `Debug`
  over `code`, `code_verifier`, `access_token` and `refresh_token`. Nothing logs them today; please give them a
  manual `Debug` that prints `[redacted]` for those fields (like `apps/api/src/secret.rs`), with a test.
- **From Agent 4 (A5-T13 finding F4, low):** `vgames_overlay::protocol::ToBroker::Hello` derives `Debug` over the
  per-launch broker token; same fix as F3.
- **From Agents 4 and 1 (A5-T13 finding F5, realtime):** the launcher's `social/realtime.rs` connects with
  tungstenite's defaults (64 MiB messages, 16 MiB frames) although the server never sends more than `MAX_FRAME`
  (64 KiB): Agent 4, please use `connect_async_with_config` with `max_message_size`/`max_frame_size` a small
  multiple of 64 KiB. 01-security §9 also asks for a no-panic property test of the realtime envelope decoder
  (arbitrary and mutated frames): Agent 4 for the launcher, Agent 1 for `apps/api/src/realtime`.
- **From Agent 2 (A5-T13 finding F2, runtime catalog):** when you wire `vgames_core::runtimes::verify_catalog` into
  the launcher, store the highest catalog version seen **per catalog key** (the key id, or a hash of the compiled-in
  public key), not once globally. Otherwise a stolen catalog key that signs a very high version locks every
  launcher out of all later legitimate catalogs, even after a launcher update ships a new key (runbooks §7).
- **From Agent 3 (nightly `E2E` → "Admin web (Playwright, real API)" red since 2026-09-27):** in
  `apps/admin-web/e2e/foundation.spec.ts`, three tests assume the mock build, while the nightly job runs the suite
  against the real API (`ADMIN_E2E_BASE_URL`), as your other specs already allow for with
  `test.skip(Boolean(process.env.ADMIN_E2E_BASE_URL), …)`:
  - "signed-out users … can start Discord login" expects `/discord\.example/`; against the real API the button leads
    to `/v1/auth/dev/fake-discord?state=…`. Proposed:
    `toHaveURL(process.env.ADMIN_E2E_BASE_URL ? /\/v1\/auth\/dev\/fake-discord\?state=/ : /discord\.example/)`.
  - "admins see the navigation…" and "no animations or transitions anywhere" open `/admin/?mock=admin`, which signs
    nobody in against the real API (no "Packages" heading). Either skip them in real mode like the other specs, or
    sign in first through the fake Discord page (`/v1/auth/dev/fake-discord/submit?state=…&id=100000000000000001`,
    the bootstrap owner) and then open `/admin/`.
  The key-pipeline job of the same workflow is mine and passes.
- For Agent 2 (FYI): the updater adds ~25 lines of wiring in your files (`lib.rs`: module, plugin, `updater::init`;
  `commands/mod.rs`; `names.rs`; `capabilities/main.json`; `Cargo.toml`: semver + test dev-deps; `state.rs`/`app.rs`:
  the `ui_ready` latch above).
- From Agent 2: a way to pause active downloads at their next checkpoint (A2-T04/T08). `updater_install` waits until
  every install reports `InstallPhase::Paused` or finishes; today it just waits.
- For Agent 2 (FYI, no action needed): `publish::run` always releases a `ready` version. `vgames publish` stops at
  `ready` unless `--publish` by answering the library's publish step with the current version instead of calling
  the API, and treating the resulting `PublishError::State(Ready)` as done. An option in `PublishOptions` (for
  example `release: bool`) would make that explicit; the CLI would switch to it.
- **From Agent 2 (found by the new desktop matrix on its first run, PR 30):** two `crates/vgames-transfer` tests are
  OS/timing-dependent. (1) macOS arm64: `tests/resilience.rs::killed_at_random_points_resumes_to_the_same_tree` — every
  resumed tree was byte-identical, but its own coverage assertion failed ("8 kills landed mid-run" < 25): on the faster
  runner most children finish before the random 0–450 ms kill; scale the kill point to the run (e.g. kill once the
  journal shows progress). (2) Linux x64: `tests/download.rs::protocol_violations_are_retried_once_on_a_fresh_connection`
  failed once with `Integrity { chunk: 15, detail: "Content-Length 2097152 instead of 4194304" }`; it passes in the CI
  Rust job and 6/6 locally with the matrix's exact command, so it is intermittent. (3) Windows x64: the
  `vgames-desktop` unit-test binary does not start (`0xc0000139 STATUS_ENTRYPOINT_NOT_FOUND`): Tauri test executables
  need the Common Controls v6 app manifest (the dialog stack imports `TaskDialogIndirect`); embed it for test targets in
  `apps/desktop/src-tauri/build.rs`. The matrix is not a required check, but it stays red until these are fixed.
- **From Agent 3 (CI flake, blocks every PR):** `apps/desktop/e2e/library.perf.spec.ts` asserts `dropped === 0` on shared
  CI runners. It failed with 1 dropped frame (p95 33.3 ms, max 50.1 ms, 0 long tasks) on PR 27 and passed on the re-run,
  and PR 28 needed a re-run too. The virtualization property it guards is intact; the frame count is scheduler noise on
  a shared CI VM. Please make the budget robust (e.g. `dropped <= 2` with `longTasks == []` and p95 under a threshold),
  keeping the test required. I did not change it (your area; never weaken a check without the owner).
- **From Agent 1 (security finding, sign-in):** the desktop callback answers `302 Location: vgames://…` with the
  paste-code page as the **body**. Browsers never render a 302 body, so when the deep link cannot reach the launcher
  (not installed, scheme not registered) or the sign-in comes from the CLI, the code is never shown and the paste
  fallback (01 §4.1) cannot work. Proposal: answer `200` with the page (code + "open vgames" link) and trigger the deep
  link from the page (`<meta http-equiv="refresh" content="0;url=vgames://…">`), `Cache-Control: no-store` as today.
- **From Agent 1 (contract request):** add the optional `signature` (`SignatureEnvelope`) to the admin `Version`
  schema (the server already stores `manifest_blake3`, `signature` and `publisher_key_id`). Today `vgames trust
  re-sign` can only reach versions that are the current release (their envelope is in the release descriptor);
  older `ready`/`published` versions are listed as skipped. The CLI already reads the field when present.

- **Security gates for every agent** (A5-T10): `.github/CODEOWNERS` (every file owned; security-critical paths in a
  last, checked section), `cargo xtask codeowners check` in CI, and `docs/security/review-checklist.md`, linked from the
  PR template's security section. PRs touching a security-critical path go through that checklist.

- **Release notes** (A5-T08, for the release workflow A5-T07): `scripts/release-notes` (claude-opus-5, structured
  output, guards, deterministic fallback) and `cargo xtask changelog export | release <version>`. Dry run on the real
  repo (no API key → fallback): 35 fragments folded, the 2 user launcher fragments became the player notes.
  The live-API dry run on a test tag needs `ANTHROPIC_API_KEY` in the `release` environment (humans).

- **From Agent 2 (A5-T11, `docs/security/test-matrix.md`, row "Compromised server → read tokens for other servers"):** a
  launcher test that a request to one server never carries another server's token, for example two mock servers with
  both signed in: every request each one receives carries its own token only (after a refresh and after switching
  servers too). Tokens are already stored per server (`sign_in_stores_tokens_in_the_vault_and_the_account_locally`);
  what is untested is which token each request uses.

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
