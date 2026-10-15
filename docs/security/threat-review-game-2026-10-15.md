# Threat review: social, overlay and compatibility, 2026-10-15

Owner: GAME (GAME-14). Co-signed by INT (below). Scope: everything GAME ships in phase 2: social (friends, presence,
typing, end-to-end messages, invites, realtime), the in-game overlay (broker, renderers, Vulkan layer, overlay window)
and compatibility (runtime catalog and store, profiles, overrides, prefixes, PE scan, the compat commands). Checked
against `main` at 09796d2 plus https://github.com/wouhliss/vgames/pull/279 (fix for O1), with the items of
[review-checklist.md](review-checklist.md) and the threat model of 01-security §1. Each answer names a test that fails
if the rule breaks, or the code read for this review.

**Verdict.** No security invariant is broken. One medium finding was an open abuse path and is fixed in PR 279 (O1:
a game could act as the player through the overlay). One medium finding is a release gate outside GAME's code (C1: the
runtime catalog key and signing workflow are not on `main`, so release launchers run no compat runtime). The rest are
hardening (O2–O4, C2–C5, S1–S5) and notes. The three earlier findings in this scope (F2, F4, F5 of
[review-2026-09-30.md](review-2026-09-30.md)) are fixed.

## Findings

| Id | Owner | Severity | Status | Finding |
|---|---|---|---|---|
| O1 | GAME | Medium | **Fixed** (PR 279) | The game holds the broker token by design, and the launcher ran any `QuickReply`, `AcceptInvite` or `DeclineInvite` a renderer sent, for any id, panel open or not: a modded game could message the player's friends or accept invites as them. Now these run only while the panel is open and shows that conversation or invite (`overlay::tests::only_the_open_panel_lets_actions_act_for_the_player`). |
| C1 | INT (A5-T12 carry-over) | Medium, **release gate** | Open | `runtimes/runtime-catalog.pub`, the catalog signing workflow and `scripts/runtimes/` are not on `main` (only on `agent5/d3dmetal-intake`). `build.rs` falls back to no key, so release builds report the catalog `Off` and never install a runtime. Fail-closed, not an exploit; before a release, land the key and workflow and re-run "Runtimes only from pinned URLs" on them. |
| C2 | GAME (`contract:` PR, `vgames-core`) | Low–Medium | Open | `is_denied_env_key` (`vgames-core/src/manifest.rs`) refuses `PATH`, `LD_PRELOAD`, `LD_LIBRARY_PATH`, `PYTHONPATH`, `COMSPEC` and `DYLD_*`, but not `LD_AUDIT` and the other `LD_*` keys, `GCONV_PATH`, `PYTHONHOME`, `BASH_ENV`/`ENV`, or the Vulkan loader keys (`VK_ICD_FILENAMES`, `VK_LAYER_PATH`, `VK_ADD_LAYER_PATH`, `VK_INSTANCE_LAYERS`). A signed manifest or profile can load a library into the host-side `umu-run` (Python) or pressure-vessel. Only a trusted publisher can sign these, and a publisher already ships the game's code, so invariant 2 holds; the denylist should still mean what it says. Deny the `LD_` prefix and the keys above. |
| O2 | GAME (`contract:` PR) | Low | Open | 01-security §7 and 05-social §6.1 say the broker takes one connection and closes the listener after the handshake. The broker re-listens on the same port with the same token after the renderer leaves (`broker.rs`, `rebind`), so the renderer can reconnect after a hiccup. While the port is free, another local process could bind it and read the renderer's `Hello`. Either let the renderer verify the broker (a proof under the token in `Welcome`) or document reconnection in the contract. |
| O3 | GAME | Low | Open | Handshakes are served one at a time with a 5 s timeout, and five failures stop the broker for that game: any local process can switch the overlay off for the session (the launcher is unaffected). Serve handshakes concurrently with a small cap and do not count timeouts as failed authentication. |
| O4 | GAME | Low | Open | Broker writes (`wr.write_all`) have no timeout: a renderer that stops reading blocks the serve loop until the socket closes, so `DEAD_AFTER` and the stop token are not seen. Wrap writes in a timeout or race them with the stop token. |
| C3 | GAME | Low | Open | `PROTON_LOG` and `PROTON_LOG_DIR` are not launcher-owned: a profile can turn on verbose Proton logs and pick their directory. Make them launcher-owned and set them only from a local diagnostics switch (the E2E job would set the switch). |
| C4 | GAME | Low | Open | `rosetta_install` runs `softwareupdate --install-rosetta --agree-to-license` on the WebView's word; the confirmation is the UI's. Ask natively in Rust before running it. |
| C5 | GAME | Low | Open | Runtime downloads time out on connect and on each body chunk, not on the response headers (`compat/runtimes.rs`, `send()`), and the store holds its busy lock meanwhile: a stalled host blocks GC, `runtime_remove` and compat launches until the connection dies. Wrap `send()` in a timeout. |
| S1 | GAME | Low | Open | The launcher keeps any length of `presence.changed.package_title` up to the realtime frame cap and passes it to the UI and overlay; an honest server limits it to 200 characters. Drop or truncate longer titles and control characters in Rust. |
| S2 | GAME | Low | Open | Rust emits `presence_changed` for any user and `typing` for any conversation; only React filters them to friends and members. Filter in Rust (friends list, member cache) as defense in depth. |
| S3 | GAME (`contract:` PR, `vgames-proto`) | Low | Open | The join-secret grammar accepts a leading `-`, so a secret like `--windowed` is passed as one argument but a lenient game parser may read it as a flag. Require the first character to be alphanumeric (or `[`). |
| S4 | GAME | Low | Open | The 0600 secrets fallback file is created and then written in place; a crash in between leaves a short file that loses the chat keys, and the mode is not re-checked on read. Write to a temp file, fsync, rename; refuse a mode other than 0600. On Windows the file relies on the AppData ACL; say so in the Settings warning. |
| S5 | GAME (API) | Low | Open | Inbound realtime frames other than `typing` and `presence.set` (`ping`, unknown types answered with `error`) are not rate-limited, so a client can make the server answer at socket speed. Check `Policy::User` (or a per-connection bucket) for every frame. |
| N1 | GAME | Info | Open | Hardening notes, no fix required for release: `RealtimeTicket` derives `Debug` over a single-use ticket (same pattern as F3); the log redactor matches `token` as a whole word, so `VGAMES_OVERLAY_TOKEN=…` would pass if someone logged it (nothing does); the Vulkan layer manifest is written through symlinks and stays after an AppImage run whose mount path is temporary; the launcher resyncs over REST on every `friend.*` and `inbox.new` event without coalescing; the server's typing throttle is per API instance; the launcher's 3 s typing throttle has no test asserting one frame. |

## Earlier findings in this scope

| Id (2026-09-30) | Status | Evidence |
|---|---|---|
| F2: highest runtime-catalog version per key | Fixed | `compat/catalog.rs` keys the stored version by `key_id`; `compat/catalog/tests.rs::a_high_version_under_one_key_never_blocks_another_key`, `::an_older_catalog_after_a_newer_one_is_a_rollback`, `::an_older_read_never_replaces_a_newer_accepted_catalog` |
| F4: overlay `Hello` prints the token | Fixed | Manual `Debug` in `vgames-overlay/src/protocol.rs`; `::hello_debug_never_prints_the_token` (both `{:?}` and `{:#?}`); both `Endpoint` types redact too |
| F5: realtime frame limits and no-panic tests | Fixed | Launcher: `max_message_size`/`max_frame_size` at 4 × `MAX_FRAME`, `social/realtime.rs::a_2_mib_frame_is_refused_from_its_header`, `::decode_envelope_never_panics_on_arbitrary_text`, `…_on_arbitrary_bytes`, `…_on_mutated_frames`. API: `realtime/mod.rs::arbitrary_realtime_bytes_never_panic`, `::mutated_realtime_frames_never_panic`, `::oversized_valid_realtime_frames_are_rejected` |

## Social

| Threat | Answer | Evidence |
|---|---|---|
| Plaintext reaches the server (invariant 3) | No | `apps/api/tests/it/social_relay.rs::plaintext_never_reaches_the_database` (real vodozemac sessions); no `tracing` call in the launcher's `social/` or the API's `social/` and `realtime/` logs a body or payload |
| Keys and bodies readable at rest | No | Pickles and bodies encrypted under keychain keys: `social/store/tests.rs::bodies_and_pickles_are_encrypted_at_rest`, `::state_survives_a_restart_and_needs_the_keychain_key`, `social/crypto/tests.rs::body_cipher_binds_the_row`; 0600 fallback `secrets.rs::file_store_creates_once_with_private_permissions` (S4) |
| Forged devices or one-time keys | Refused | Server: `social_devices.rs::registration_binds_the_session_and_checks_the_self_signature`, `::one_time_key_uploads_check_every_signature_and_the_cap`. Launcher: `store/tests.rs::a_device_with_a_bad_self_signature_is_never_pinned`, `crypto/tests.rs::one_time_key_signatures_cover_the_fallback_flag` |
| Server swaps a friend's device key | Visible, blocks sending | `store/tests.rs::a_changed_key_blocks_sending_until_trusted`, `::safety_numbers_match_and_device_changes_need_reverification`, `crypto/tests.rs::safety_numbers_are_symmetric_and_change_with_devices` |
| Forged sender, replays, tampered ciphertext | Refused | `store/tests.rs::unknown_sender_is_reported_until_its_device_is_pinned`, `::duplicates_are_ignored`, `::tampered_ciphertext_is_rejected_and_harmless`, `crypto/tests.rs::out_of_order_delivery_decrypts_and_duplicates_fail_closed` |
| Malicious friend: oversized or malformed payloads | Refused | `social/payload.rs::oversized_input_is_refused`, `::unknown_or_invalid_content_is_unsupported_never_interpreted`, `store/tests.rs::oversized_ciphertext_is_refused_before_decoding`, `social_relay.rs::size_and_shape_limits` |
| Malicious friend: launch-argument injection | Refused (S3 hardening) | `vgames-proto/src/social.rs::join_secret_grammar`, `payload.rs::an_invalid_join_secret_is_dropped_not_interpreted`, `launch/target/tests.rs::join_secret_is_one_whole_argument` |
| Malicious friend: spam, blocks | Limited, enforced by the server | `social_friends.rs::friend_code_creation_and_requests_are_rate_limited`, `::blocks_are_symmetric_and_look_like_unknown_users`, `::blocking_cancels_active_invites`, `social_invites.rs::invite_creation_is_rate_limited`, `social_relay.rs::sends_are_rate_limited_per_device` |
| Presence and typing reach non-friends | No | `social_presence.rs::presence_reaches_accepted_friends_only`, `social_relay.rs::typing_reaches_the_other_members_only`; typing throttled per (user, conversation): `typing.rs::one_fan_out_per_user_and_conversation_every_three_seconds` (S5, N1) |
| Malicious server: huge frames, decoder crashes | Bounded, no panic | F5 above; inbox HTTP body cap `api.rs::a_100_mib_body_is_refused_at_the_cap`; `crypto/tests.rs::mutated_ciphertexts_never_panic`, `payload.rs::arbitrary_bytes`, `::mutated_payloads` |
| Malicious server: forged presence or typing text | Plain text only (S1, S2) | React text nodes only (`MessageList.tsx`, `friends/model.ts`), no `dangerouslySetInnerHTML` in `apps/desktop/src` |
| Secrets reach the WebView | No | The social model carries fingerprints, safety numbers and `has_join_secret`, never a ticket, token, pickle or join secret (`social/model.rs`) |

## Overlay

| Threat | Answer | Evidence |
|---|---|---|
| Remote connection to the broker | Impossible | Binds `127.0.0.1:0` and drops non-loopback peers (`overlay/broker.rs`); the renderer refuses a non-loopback endpoint: `vgames-overlay/src/link.rs::the_environment_must_name_a_loopback_broker_and_a_token` |
| Another local process impersonates the renderer | Needs the per-launch token | 32 bytes from `getrandom` per launch, constant-time compare: `protocol.rs::tokens`, `broker.rs::wrong_tokens_are_refused_and_the_broker_gives_up_after_five` (O2, O3) |
| The game acts as the player | Only through the open panel, on shown items | O1, PR 279 |
| Malformed or huge frames from the game | Refused before allocation, no panic | `protocol.rs::oversized_empty_and_trailing_frames_are_refused_before_allocation`, `::decoding_arbitrary_bytes_never_panics`, `::caps_are_enforced`; `broker.rs::oversized_frames_drop_the_link_and_the_renderer_can_reconnect` |
| A flood of actions or a slow renderer stalls the launcher | No | Bounded queues: `broker.rs::events_nobody_reads_are_dropped_and_never_block_the_broker`; 5 s hello and 15 s heartbeat deadlines (O4 for writes) |
| Token leaks | Only to the game, by design | Redacting `Debug` (F4); no log statement prints the plan's environment (`launch/plan/tests.rs` asserts the child sees no launcher token) |
| Layer loads unauthenticated code (invariant 1) | No | The manifest points at the bundled library under the updater-signed install: `vulkan_layer.rs::the_library_is_looked_up_under_the_bundled_overlay_directory`; written only under `$XDG_DATA_HOME`: `::the_manifest_directory_follows_xdg` |
| Layer affects other Vulkan apps | No | Loads only with `VGAMES_OVERLAY=1`: `vulkan_layer.rs::the_manifest_loads_only_with_the_overlay_variable`; `prepare_launch` sets it only when the overlay is on for that game |
| Renderer crashes or slows the game | Contained | `guard.rs::a_panic_in_a_hook_disables_the_overlay_without_unwinding_into_the_caller`; frame budget `gl.rs::a_window_over_budget_turns_the_overlay_off`, `vulkan.rs::the_budget_trips_only_on_a_slow_average`; two quick crashes turn it off for the game: `valve.rs::two_quick_crashes_in_a_row_trip_and_re_enabling_resets`. Every `unsafe` block in `gl.rs` and `vulkan.rs` has a `SAFETY` comment |
| Server text in the overlay runs as code | No | JSX text only in `src/overlay/` (`OverlayApp.test.tsx` "… as plain text"); in game, `View::validate` refuses control characters; the overlay window's capability grants only `overlay_view`, `overlay_action` and events, under `default-src 'self'` |
| Anti-cheat | Out of scope | 01-security §1: packages have none and the overlay relies on that |

## Compatibility

| Threat | Answer | Evidence |
|---|---|---|
| Forged or rolled-back runtime catalog | Refused | Prehashed minisign under the compiled-in key, overrides only in debug: `compat/catalog/tests.rs::release_builds_ignore_the_overrides`; rollback per key (F2); stored copies re-verified: `::a_stored_catalog_that_no_longer_verifies_is_ignored`; size caps while reading: `::oversized_bodies_are_refused_while_reading` (C1) |
| Runtime from an unpinned URL or with other bytes (invariant 1) | Refused before extraction | GitHub release assets only: `vgames-core` `runtimes` `rule_url_is_a_github_release_asset`; `compat/runtimes/tests.rs::a_tampered_archive_is_refused_before_anything_is_extracted`, `::the_pinned_size_is_enforced_while_downloading`; end to end with real Proton: `compat-e2e.yml` flips one pinned hash and `compat/plan/proton_e2e.rs` asserts `refused-before-extraction` |
| Hostile archive escapes the runtime directory | Refused | `compat/extract/tests.rs::parent_components_are_refused`, `::absolute_paths_are_refused`, `::a_link_chain_leaving_the_target_is_refused_in_either_order`, `::nothing_is_written_through_an_extracted_link`, `::device_nodes_and_fifos_are_refused`, `::hard_links_only_to_extracted_files`, `::zip_archives_follow_the_same_rules`, `::arbitrary_archives_never_panic`; setuid bits stripped |
| Poisoned `.part` resume, removal outside the store | Neither | A resumed part still passes the full SHA-256: `runtimes/tests.rs::an_interrupted_download_resumes_with_a_range_request`; `::an_escaping_link_leaves_no_runtime_behind`, `::garbage_collection_keeps_referenced_versions`, `::a_version_in_use_cannot_be_removed` (C5) |
| Server forges or replays a compat profile (invariant 2) | Refused | `compat/profiles/tests.rs::a_profile_signed_by_a_revoked_key_is_ignored_with_that_status`, `::a_stored_profile_stops_applying_once_its_key_is_revoked`, `::an_older_revision_is_refused_and_the_stored_one_stays`, `::a_tampered_stored_document_is_not_used`, `::without_trust_nothing_is_accepted` |
| Profile or player override sets a dangerous variable | Refused for the listed keys (C2, C3) | Launcher-owned keys and `is_denied_env_key`, checked on parse, on save, on load and again at spawn: `rule_env_manifest_rules_plus_launcher_owned`, `unknown_keys_and_launcher_owned_env_keys_are_refused`, `compat/overrides` `env_rules`; DLL overrides from validated names only: `rule_dll_overrides`; `env_clear()` plus a host allowlist in `launch/plan.rs` |
| Prefix path traversal | Impossible | `compat::prefix_dir` joins two UUIDs; removal only on uninstall through `remove_tree_no_follow` |
| Malformed game executable crashes the PE scan | No | Bounded reads and counts in `compat/pe.rs`; `pe/tests.rs::arbitrary_bytes_never_panic`, `::mutated_executables_never_panic` |
| WebView drives compat with arbitrary paths | No | `runtime_remove` takes a `RuntimeId` and an installed version; `compat_override_set` checks the package and the overrides; `compat_licenses` reads only installed runtime directories, capped, without following links (C4) |
| Secrets in compat logs | None | Network errors drop the URL (`without_url`); Wine runs with `WINEDEBUG=-all`; the launcher never enables Proton logging itself (C3) |

## Co-signature

- GAME: this review, 2026-10-15.
- INT: _pending — INT reviews GAME's code directly (int-integrator.md "The threat review to co-sign") and signs here
  in this PR, or files what it disagrees with in `docs/agents/status/int.md`._
