# Security review checklist

Owner: Agent 5. Run it for every PR that touches a security-critical path (the `SECURITY-CRITICAL`
section of [`.github/CODEOWNERS`](../../.github/CODEOWNERS)) and for every release. Each item maps to an
invariant or rule in [01-security](../architecture/01-security.md). Answer each with a yes, a "not touched",
or a finding filed in the owning agent's status file.

## Invariant 1: nothing unauthenticated is executed or written into an install

- [ ] Package bytes are checked against the signed manifest **before** they reach their file (chunk BLAKE3 in memory, 02 §7.5).
- [ ] The manifest signature is verified with `vgames_core::verify::verify_manifest` before any file is created; nothing re-serializes manifest JSON before verifying.
- [ ] Every path used for writing passed `vgames_core::paths` and the launcher's canonicalize-under-root check (no symlink/junction escape).
- [ ] Runtimes are downloaded only from catalog-pinned URLs and verified by SHA-256 before extraction.
- [ ] The updater installs only minisign-verified artifacts, never downgrades, and never interrupts an install or a running game.
- [ ] zstd decoding is bounded to the declared chunk size.

## Invariant 2: a compromised server cannot make launchers run arbitrary code

- [ ] No signing key, key file or passphrase reaches the server, its logs or its database.
- [ ] Trust bundles are verified under the **pinned** root (`verify_bundle`), with version monotonicity and `next_root` as the only rotation path.
- [ ] Revocation is honored everywhere a signature is checked (install, update, pre-launch, server finalize and re-sign).
- [ ] Launch arguments and environment come only from the signed manifest or the signed compat profile, never from API metadata, deep links or invites.

## Invariant 3: message plaintext never leaves the endpoints

- [ ] Social tables and message relays store and log ciphertext only (no plaintext columns, no debug logging of payloads).
- [ ] Olm pickles and chat-DB keys stay in the OS keychain (or the documented 0600 fallback with a visible warning).

## Invariant 4: the WebView holds no secrets and has no direct network or file access

- [ ] No new Tauri capability grants generic fs, shell, http or opener access to the WebView.
- [ ] CSP in `tauri.conf.json` is unchanged or stricter (`connect-src` IPC only, no remote script).
- [ ] New commands validate every argument in Rust (UUIDs, closed enums, paths under a library root).
- [ ] No token, key or signed URL is returned to the WebView.
- [ ] Server-provided text is rendered as plain text or through the safe Markdown renderer (never `dangerouslySetInnerHTML`).

## Invariant 5: privileged server actions are authenticated, authorized, rate-limited and audit-logged

- [ ] New admin/owner routes use `RequireAdmin` / `RequireOwner` and take identity from the session, never from the body.
- [ ] Each new route has a rate limit, and each mutation writes `audit_log` in the same transaction.
- [ ] Cookie-authenticated unsafe methods require the CSRF header **and** a matching `Origin`.
- [ ] Refresh tokens rotate on use and reuse revokes the session.

## Deep links, invites, join secrets

- [ ] Deep links go through the strict parser; unknown forms are ignored and logged; `server/add` and `auth/callback` need confirmation or a pending flow; `launch` is rate-limited and runs only installed, verified packages.
- [ ] Join secrets match `^[A-Za-z0-9._:\-\[\]]{1,256}$` and replace only a whole `{join_secret}` argument.

## Cryptography and secrets

- [ ] Only the primitives of 01-security §2 are used (no new algorithm without a `contract:` PR).
- [ ] Randomness comes from the OS CSPRNG (`getrandom`), never a userspace PRNG.
- [ ] Secrets are compared in constant time and zeroized after use; `Debug` output never prints them.
- [ ] Logs redact `Authorization`, cookies, `code`, `state`, tokens and signed URL query strings.

## Untrusted input

- [ ] Every parser of untrusted bytes has size limits and a no-panic property test (arbitrary and mutated input).
- [ ] No `unwrap`/`expect`/`panic!`/indexing on untrusted data outside tests; no new `unsafe` except FFI with a `// SAFETY:` comment.
- [ ] Outbound fetches from the server keep the SSRF rules (HTTPS, host allowlist, no private ranges, size and time caps).

## Supply chain and CI

- [ ] New dependencies pass `cargo deny` (license allowlist, crates.io only) and `pnpm audit --prod`; lockfiles are committed.
- [ ] Workflows keep actions pinned by commit SHA, `permissions: {}` at the top, no secrets in PR workflows.
- [ ] Release jobs still require the `release` environment (human approval).
- [ ] `gitleaks` is green and no fixture contains a real token, key file or signed URL.
