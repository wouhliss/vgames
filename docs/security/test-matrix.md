# Security test matrix

Every security invariant and every threat in [01-security](../architecture/01-security.md) (top of the file, and
§1), plus each attack A5-T11 names, mapped to the automated tests that would fail if it stopped holding.
Owner: INT. When you change one of these behaviours, keep its row pointing at a test that still fails
without it.

**Where each test runs.** Everything below runs at least nightly.

| Tag | Workflow | When |
|---|---|---|
| **CI** | `ci.yml` job "Rust (fmt, clippy, tests)": `vgames-core`, `-pack`, `-transfer`, `-cli`, `-proto`, `xtask`, and the API integration tests on PostgreSQL | every PR, every push to `main`, nightly 02:13 UTC |
| **CI gates** | `ci.yml` job "Changelog fragments": `cargo xtask security check` and `cargo xtask codeowners check` | same |
| **CI TS** | `ci.yml` job "TypeScript": Biome (lint rules) and Vitest | same |
| **CI launcher** | `ci.yml` job "Launcher UI end-to-end (mock mode)": `apps/desktop/e2e` (Playwright) | same |
| **CI admin** | `ci.yml` job "Admin UI end-to-end (mock mode)": `apps/admin-web/e2e` (Playwright) | same |
| **Desktop** | `desktop-matrix.yml`: `cargo test -p vgames-desktop` and the crates it ships, on Windows, Linux and macOS | nightly 02:23 UTC, and PRs touching the launcher |
| **E2E** | `e2e.yml`: `scripts/e2e/key-pipeline.sh` (`vgames` CLI against the real API) and the admin suite against the real API | nightly 03:41 UTC |

Test names are `file::function` (Rust) or `file › test title` (Playwright, Vitest).

## 1. Invariants (release blockers)

| # | Invariant, and the case tested | Tests | Runs |
|---|---|---|---|
| 1 | **No unauthenticated byte is executed or written into an install** | | |
| 1a | A tampered pack byte is never written; the install fails at that chunk | `crates/vgames-transfer/tests/download.rs::a_flipped_byte_is_never_written_and_is_reported`; `crates/vgames-transfer/src/download/scheduler.rs::second_mismatch_fails_with_integrity`; server side, `apps/api/tests/it/releases.rs::a_corrupted_byte_fails_verification_at_its_chunk` | CI, Desktop |
| 1b | Swapped or forged manifest: the signature check fails before anything is parsed | `crates/vgames-core/src/verify/tests.rs::step3_swapped_manifest_bytes`, `::step3_forged_envelope_digest` | CI, Desktop |
| 1c | The installed executable changed since it was verified: launch blocked | `apps/desktop/src-tauri/src/launch/orchestrate/tests.rs::a_modified_executable_is_blocked`; `launch/prelaunch/tests.rs::tampered_metadata_fails_integrity` | Desktop |
| 1d | Symlinks and paths out of the install are refused | `crates/vgames-core/src/paths/tests.rs::accepted_paths_never_escape` (property); `crates/vgames-core/src/verify/tests.rs::step6_path_traversal_manifest_refused`; `crates/vgames-transfer/tests/download.rs::a_planted_symlink_is_refused`; `crates/vgames-transfer/src/fsutil.rs::refuses_planted_symlinks`; `apps/desktop/src-tauri/src/launch/target/tests.rs::symlinks_out_of_the_install_are_refused` | CI, Desktop |
| 1e | Compatibility runtimes: a tampered catalog or archive, another key and a catalog rollback are refused | `crates/vgames-core/src/runtimes/tests.rs::tampering_another_key_and_rollback_are_refused` | CI, Desktop |
| 1f | Launcher updates (and the overlay binaries they carry): a tampered artifact, an old signed build replayed as new, and an older version are refused | `apps/desktop/src-tauri/src/updater/tests.rs::a_tampered_artifact_is_rejected`, `::an_old_signed_build_replayed_as_a_new_version_is_rejected`, `::an_older_or_equal_version_is_never_offered` | Desktop, CI (updater tests in "Desktop build check") |
| 2 | **A compromised server cannot make launchers run arbitrary code** | | |
| 2a | Bundles verify only under the root pinned at first contact; a changed root blocks the server with no bypass | `apps/desktop/src-tauri/src/servers/tests.rs::first_connection_pins_the_root_on_confirmation`, `::a_changed_root_key_blocks_the_server_without_bypass`; `crates/vgames-core/src/trust/tests.rs::signature_must_come_from_the_pinned_root`; `crates/vgames-cli/tests/server.rs::login_refuses_a_different_root_fingerprint_before_signing_in` | CI, Desktop |
| 2b | Forged bundles and bundle rollback are refused | `apps/desktop/src-tauri/src/servers/tests.rs::trust_bundle_rollback_and_forgeries_are_refused`; `crates/vgames-core/src/trust/tests.rs::version_never_goes_down`; E2E: `key-pipeline.sh` "replaying v1 is refused" | CI, Desktop, E2E |
| 2c | Manifests must be signed by a publisher key in the bundle (1b, 1d and 5a–5c apply to anything the server sends) | `crates/vgames-core/src/verify/tests.rs::step2_revoked_key_fails_everywhere`, `::revoked_key_and_holder_rules_apply` | CI |
| 3 | **Message plaintext never leaves the endpoints** | | |
| 3a | The database only ever holds ciphertext (a scan of the social tables after real traffic) | `apps/api/tests/it/social_relay.rs::plaintext_never_reaches_the_database` | CI |
| 3b | Tampered ciphertext and replayed messages fail closed on the device | `apps/desktop/src-tauri/src/social/crypto/tests.rs::tampered_ciphertext_is_rejected`, `::out_of_order_delivery_decrypts_and_duplicates_fail_closed`, `::body_cipher_binds_the_row` | Desktop |
| 4 | **The launcher WebView holds no secrets and has no direct network or filesystem access** | | |
| 4a | CSP: scripts from the app only (no inline, no eval); connections to IPC only; nothing from remote origins; no plugins, frames, forms or `<base>`; prototype frozen; asset protocol (file access) off | `xtask/src/security.rs::csp_rules` and `cargo xtask security check` on the real `tauri.conf.json` | CI gates |
| 4b | Capabilities: only core defaults and vgames' own commands, never an fs/http/shell/dialog/opener permission or a remote URL; each window gets exactly its command list | `xtask/src/security.rs::capability_rules` and `cargo xtask security check`; `apps/desktop/src-tauri/src/commands/mod.rs::main_window_gets_every_main_command`, `::overlay_window_gets_only_overlay_commands` | CI gates, Desktop |
| 4c | Tokens live in the OS vault, never in the WebView | `apps/desktop/src-tauri/src/servers/tests.rs::sign_in_stores_tokens_in_the_vault_and_the_account_locally` | Desktop |
| 5 | **Every privileged server action is authenticated, authorized, rate-limited and audit-logged** | | |
| 5a | Every admin route: anonymous, user, admin and owner get exactly their access | `apps/api/tests/it/admin.rs::authorization_matrix_covers_every_admin_route`; admin UI: `apps/admin-web/e2e/authorization.spec.ts › authorization matrix: admin`, `› authorization matrix: owner` | CI, CI admin |
| 5b | Every route of the generated OpenAPI document is rate-limited (no credentials, a junk token, a malformed header, a valid session) | `apps/api/tests/it/limits.rs::every_route_is_rate_limited` | CI |
| 5c | Privileged actions are audit-logged, and the log filters and pages | `apps/api/tests/it/admin.rs::role_changes_follow_the_owner_rules` (asserts the audit rows), `::audit_log_filters_and_pages` | CI |

## 2. Threat model (01-security §1)

| Adversary → must not be able to | Tests | Runs |
|---|---|---|
| **Network attacker** → read tokens or content (TLS) | Plain http refused except loopback: `apps/desktop/src-tauri/src/servers/discovery.rs::plain_http_is_refused_except_loopback_in_debug`; `crates/vgames-cli/src/server.rs::origins`; `crates/vgames-transfer/src/upload/protocol.rs::urls_reject_plaintext_non_loopback_credentials_and_fragments`; `apps/desktop/e2e/onboarding.spec.ts › server address › http:// refused` | CI, Desktop, CI launcher |
| → swap package bytes | 1a, 1b | |
| → impersonate a pinned server | 2a; `apps/desktop/src-tauri/src/servers/tests.rs::a_link_fingerprint_must_match_the_server` | Desktop |
| **Compromised server** → get a launcher to run unsigned or tampered code | Invariants 1 and 2 | |
| → read messages | 3a, 3b | |
| → read tokens for other servers | Tokens are stored per server: 4c. **Missing:** a launcher test that a request to one server never carries another server's token (asked of Agent 2 in `docs/agents/status/agent-5.md`) | Desktop |
| **Malicious admin UI content** → execute script | 4a; raw HTML is a lint error: `biome.json` `noDangerouslySetInnerHtml`; `apps/desktop/src/components/SafeMarkdown.test.tsx › never interprets raw HTML`, `› does not make unsafe URLs clickable and never loads images`, `› validates schemes` | CI gates, CI TS |
| → reach tokens | 4b, 4c | |
| **Malicious website** (`vgames://` links) → launch anything not installed and verified | `apps/desktop/src-tauri/src/deeplink.rs::launch_links_take_only_a_canonical_package_id`, `::anything_outside_the_grammar_is_ignored`, `::parsing_never_panics` (property); launches go through `launch/orchestrate/tests.rs::install_state_and_placement_are_checked_first`, `::a_revoked_key_or_missing_trust_blocks_the_launch`, `::a_verified_install_launches_and_repeats_are_rate_limited` | Desktop |
| → add a server without confirmation | `apps/desktop/src-tauri/src/servers/tests.rs::a_link_fingerprint_must_match_the_server`; `apps/desktop/e2e/onboarding.spec.ts › vgames://server/add links › a mismatching fingerprint blocks the server`, `› a matching fingerprint is confirmed by the link` | Desktop, CI launcher |
| → complete a login without the user | `apps/desktop/src-tauri/src/servers/tests.rs::the_paste_code_fallback_checks_code_and_state`, `::a_refused_sign_in_link_ends_only_its_own_flow`; `apps/api/tests/it/auth.rs::pkce_mismatch_and_code_reuse_are_rejected` | Desktop, CI |
| **Malicious friend** → inject launch arguments | Join secrets: `crates/vgames-proto/src/social.rs::join_secret_grammar`; `apps/desktop/src-tauri/src/social/payload.rs::an_invalid_join_secret_is_dropped_not_interpreted`; `apps/desktop/src-tauri/src/launch/target/tests.rs::join_secret_is_one_whole_argument`; `crates/vgames-core/src/manifest/tests.rs::rule_join_placeholder_whole_argument` | CI, Desktop |
| → spam | `apps/api/tests/it/social_friends.rs::friend_code_creation_and_requests_are_rate_limited`, `::pending_and_friend_limits`, `::blocks_are_symmetric_and_look_like_unknown_users`; `social_invites.rs::invite_creation_is_rate_limited`; `social_relay.rs::sends_are_rate_limited_per_device`, `::size_and_shape_limits`, `::blocks_close_direct_conversations_and_hide_party_members` | CI |
| **Stolen admin web session** → publish runnable code | The server accepts a manifest only when its signature verifies under a trusted key held by the caller: `apps/api/tests/it/finalize.rs::every_failure_has_its_own_code`; `crates/vgames-core/src/verify/tests.rs::step2_server_requires_the_key_holder`; `apps/admin-web/e2e/upload.spec.ts › a key that isn't in the trust bundle is explained (422 publisher_key_untrusted)` | CI, CI admin |
| → change roles | `apps/api/tests/it/admin.rs::role_changes_follow_the_owner_rules`, `::two_owners_demoting_each_other_leave_one_owner`; `apps/admin-web/e2e/authorization.spec.ts › an admin who forces an owner-only action gets a clear refusal (403)` | CI, CI admin |
| **Stolen publisher key** → stay trusted after a revocation | `crates/vgames-core/src/verify/tests.rs::step2_revoked_key_fails_everywhere`; launch blocked until re-signed: `apps/desktop/src-tauri/src/launch/orchestrate/tests.rs::a_revoked_key_or_missing_trust_blocks_the_launch`, `launch/prelaunch/tests.rs::a_revoked_key_asks_for_reverification_despite_the_cache`; re-signing: `crates/vgames-cli/tests/server.rs::re_sign_swaps_only_genuine_signatures_and_launchers_accept_the_result`; E2E: `key-pipeline.sh` (bundle v2 revokes the first key) | CI, Desktop, E2E |
| **Compromised CI** → ship a launcher without the updater key | Jobs that read a signing secret (updater, runtime catalog, Windows and Apple signing) or mint OIDC tokens must run in the approval-gated `release` environment; other secrets need an environment; no secrets at workflow level; no `pull_request_target` or `workflow_run`: `xtask/src/security.rs::workflow_rules` and `cargo xtask security check` on every workflow. The workflows themselves are security-critical code-owned paths: `cargo xtask codeowners check` | CI gates |

## 3. Attacks named in A5-T11

| Attack | Tests | Runs |
|---|---|---|
| Authentication credentials leaked by diagnostics (F3) | `crates/vgames-proto/src/auth.rs::tests::token_debug_redacts_credentials` | CI, Desktop |
| Malformed or oversized realtime envelopes crash the server (server F5) | `apps/api/src/realtime/mod.rs::tests::arbitrary_realtime_bytes_never_panic`, `::mutated_realtime_frames_never_panic`, `::oversized_valid_realtime_frames_are_rejected`, `::realtime_decoder_enforces_version_and_exact_limit` | CI |
| Tampered pack byte (install fails before writing that chunk) | 1a | CI, Desktop |
| Swapped manifest (signature failure) | 1b | CI, Desktop |
| Revoked key (launch blocked until re-sign) | "Stolen publisher key" above | CI, Desktop, E2E |
| Rollback manifest (lower sequence refused) | `crates/vgames-core/src/verify/tests.rs::step5_rollback_refused_unless_explicit`; `crates/vgames-transfer/src/update/plan.rs::rollback_needs_an_explicit_choice` | CI, Desktop |
| Trust-bundle rollback refused | 2b | CI, Desktop, E2E |
| Path-traversal manifest refused | 1d | CI, Desktop |
| Deep-link injection | "Malicious website" above | Desktop, CI launcher |
| Admin CSRF without the header, or with a foreign Origin | `apps/api/tests/it/auth.rs::web_sessions_use_cookies_with_csrf_and_origin_checks` | CI |
| Refresh-token reuse | `apps/api/tests/it/auth.rs::refresh_rotates_and_reuse_revokes_the_session`; `apps/desktop/src-tauri/src/servers/tests.rs::refresh_rotates_the_token_once_for_concurrent_callers` | CI, Desktop |
| Expired signed URLs | Launchers refresh them: `crates/vgames-transfer/tests/download.rs::expired_links_are_refreshed`. Tampered and retargeted URLs are refused by the fs storage backend: `apps/api/tests/it/storage.rs::fs_urls_enforce_their_signature`. Expired ones too (GCS enforces its own expiry): E2E `key-pipeline.sh` "expired storage links are refused; fresh ones still work", against a second API signing 60-second links (the lowest `VGAMES_SIGNED_URL_TTL_SECONDS`) | CI, Desktop, E2E |
| Plaintext scan of the social tables | 3a | CI |
| Updater tamper | 1f | Desktop, CI |

## Gaps being closed

- A launcher request to one server never carrying another server's token (row "Compromised server → read tokens for
  other servers"): asked of Agent 2.
