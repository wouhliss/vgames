# Agent 4 status

Networking & Multiplayer (`apps/api/src/social/**`, social migrations, `crates/vgames-proto/src/{social,realtime}.rs`,
the `social`/`messaging`/`invites` OpenAPI tags, `apps/desktop/src-tauri/src/{social,overlay}/**`,
`apps/desktop/src/overlay/**`, `crates/vgames-overlay/**`).

## Done
- A4-T01 — Adaptation note `docs/architecture/05-social-notes.md` (contract doc): Arachnel → vgames mapping,
  canonical JSON for device/OTK signatures, safety-number derivation, plaintext payload types, presence liveness,
  realtime events, and the full Tauri command/event surface for the social UI and the overlay window.
- A4-T02 — E2EE core (launcher, offline-testable): `social::crypto` over vodozemac 0.11 (accounts, canonical-JSON
  device/OTK signatures, 50 OTKs + fallback key, outbound/inbound sessions, encrypted pickles, safety numbers,
  XChaCha20-Poly1305 bodies), `social::store` (SQLite migration `0002_social`: accounts, sessions per device, TOFU
  device pins with key-change blocking and revocation, contact verification, history and outbox encrypted at rest,
  processed-envelope markers), `social::secrets` (keychain, `0600` file fallback), `social::payload`.
  Evidence: 32 unit tests, among them two and three in-process launchers through a fake relay: pre-key → normal
  transition, out-of-order delivery (normal and pre-key bursts), duplicates ignored (redelivered envelope and resent
  `client_message_id`), fallback key after OTK exhaustion, restart with the right/wrong keychain key, tampered
  ciphertext and substituted identity key rejected, symmetric safety numbers with re-verification after a new device,
  changed-key blocking and trust, revoked devices dropped, no plaintext in the SQLite file; no-panic property tests
  for the payload decoder, the Olm message parser, signature checks and the whole receive path (40 tests).

- A4-T03 — Server: friends, codes, blocks, profiles, presence. `apps/api/src/social/{friends,presence,relations,events}.rs`,
  migration `20260925120000_social_invites_presence.sql` (`user_presence.heartbeat_at`, the two invite columns of
  05-social-notes §3), job `social.sweep` (60 s). Blocks and invisible users answer `404 not_found` exactly like
  unknown ids; presence goes to accepted friends only; each API instance heartbeats the users with a socket on it
  every 10 s and any instance marks rows older than 30 s offline (guarded update, published once).
  Evidence: 13 API tests (`tests/it/social_{friends,presence}.rs`) including block symmetry, 404-not-403, limits
  (500 / 100), rate limits, cross-instance presence and offline after disconnect; mutation-checked.

- A4-T04 — Server: devices and key directory. `apps/api/src/social/devices.rs`: `POST /v1/devices` (desktop sessions
  only; strict Ed25519 self-signature over `canonical::device_keys` bound to user and server; binds the session;
  signing in again with the same account re-binds; keys of revoked or other users' devices refused), own list with
  key counts, revocation (deletes keys, revokes bound sessions, `device.revoked` to contacts), signed OTK/fallback
  upload (each signature checked, fallback flag signed, max 100 unclaimed, only by the device itself), atomic claims
  (`FOR UPDATE SKIP LOCKED`, fallback when exhausted, own devices / accepted friends / conversation co-members only),
  `GET /v1/users/{id}/devices`. `social.sweep` forgets claimed keys after 30 days.
  Evidence: 6 API tests (`tests/it/social_devices.rs`) incl. 100 parallel claims → 100 distinct keys and every bad
  signature case; mutation-checked (locking, signature check, relationship check).

- A4-T05 — Server: conversations and message relay. `apps/api/src/social/relay.rs`, migration
  `20260925130000_social_relay.sql` (`conversations.last_message_at`): direct get-or-create (friends, no block),
  parties (creator + 1–15 friends), activity-ordered list; sends need a device and membership, each envelope goes to
  an active device of a member or the sender (block either way → not addressable; a DM with a block → 404), ≤ 64 ×
  64 KiB (413), 120/min per device, idempotent per (sender device, client_message_id, recipient), `unknown_devices`;
  per-device inbox (oldest first, cursor), ack deletes only the caller's envelopes, `inbox.new` per recipient,
  expiry in `social.sweep`. `olm_message_type` now documents its `[0, 1]` enum in the generated OpenAPI.
  Evidence: 7 API tests (`tests/it/social_relay.rs`) incl. a real vodozemac session through the relay and a scan of
  every column of every table for the plaintext; mutation-checked (ack scope, membership, blocks).

- A4-T06 — Server: invite state machine. `apps/api/src/social/invites.rs`, migration
  `20260925140000_social_invites.sql` (sender index; the one-active unique index already existed): create (accepted
  friends; strangers and blocks → 404; published package with a release; one active per triple returns it; 60/h),
  accept/decline (invitee), cancel (sender), status (installing/ready/joined/failed per 04-database §3; progress
  stored always, published ≤ every 2 s), every transition a guarded update (racing requests: one wins, the other
  409), expiry (pending 10 min, 24 h after accept) in `social.sweep` and lazily in its own transaction, events to
  both parties; blocks cancel live invites with `invite.updated`. All social operations of the contract are now
  implemented (`openapi_unimplemented.txt` is empty).
  Evidence: 7 API tests (`tests/it/social_invites.rs`): all 63 action × state cases, races, throttling over a real
  socket, expiry, create rules, rate limit; mutation-checked (state guard, role guard, throttle).

- A4-T07 — Launcher: realtime client, social state, presence (partly; see below). `social::{ports, api, realtime,
  service, presence, netwatch, idle, commands}`: one socket per signed-in session (ticket → WebSocket → `hello` → REST
  resync → events), 60 s silence = dead, 1–60 s full-jitter backoff reset after `hello`, immediate reconnect on a
  network change (local-route sampling every 10 s) or a new session, token refreshes keep the socket, sign-out and
  server switches close it, 4001 reports the token; friends/codes/blocks (local list)/profile/settings commands and
  `social-connection-changed`, `friends-changed`, `presence-changed`, `friend-request-received`; presence from
  `GameStarted/GameStopped`, OS idle (Windows, macOS; none on Linux) and "show what I'm playing", sent on change and
  after every `hello`. Evidence: 7 tests against a fake gateway over real sockets (drops, restart on the same port,
  network change skipping a 30 s backoff, silent socket, server switch, sign-out, revoked session, presence rules,
  friend actions, local blocks, offline), mutation-checked. **Not yet:** tests against the real API across two
  instances and the 24 h idle soak need a signed-in launcher session, i.e. Agent 2's session manager (A2-T07); they
  move to A4-T12.

- A4-T08 — Launcher: messaging integration. `social/service/messaging.rs` (registration after every `hello`,
  `device_keys_in_use` → fresh account, one-time keys topped up to 50 after registration and after pre-key
  messages, outbox with backoff on one timer, inbox drain → decrypt → store → ack, `unknown_devices` and
  `400 unknown_recipient` handling, device notices, safety-number and device commands), `social/store/conversations.rs`
  (conversation cache, unread counts), `store::{prepare_top_up, replace_account, targets}`, server `social/typing.rs`
  (the realtime `typing` fan-out 05-social-notes §3 promised; it was missing). Found through the real API:
  vodozemac numbers fallback and one-time keys separately, so fallback key ids collided with one-time key ids and
  every key upload failed → fallback ids are prefixed `F` (05-social-notes §2.1).
  Evidence: `apps/desktop/src-tauri/tests/social_chat.rs` runs three launchers against the **real API in process**
  on PostgreSQL (`VGAMES_TEST_DATABASE_URL=… cargo test -p vgames-desktop --test social_chat -- --ignored`, 6–7 s,
  6/6 green runs): Alice ↔ Bob (pre-key, then normal messages), unread counts and mark-read, typing, equal safety
  numbers; Bob's second computer joins → Alice gets a `new_device` notice in the conversation, the safety number
  changes, the new device receives new messages (and copies of Bob's own) but not the three old ones; Bob revokes
  it → `device_revoked` notice, no envelope is addressed to it afterwards; no plaintext in `message_envelopes`.
  5 new store tests (fallback ids, top-up math, account replacement, send targets, conversation cache), 3 unit tests
  (error classification, refused-envelope parsing, device names), API test `typing_reaches_the_other_members_only`
  (members only, throttle, non-members, blocks, malformed frames).

- A4-T07 follow-up — social follows the real sign-in: `social::session_bridge` feeds `SessionSlot` from Agent 2's
  `Servers` (active server, account, access token; re-read on `ServerSwitched`/`ServersChanged`/`AuthFinished`),
  refreshes a refused token through `Session::refresh` (single-flight), and REST calls retry once with it
  (`SessionSlot::set_refresh_wait`). Fixed: after close 4001 the socket waited for a new *identity* and never
  reconnected with a refreshed token. Tests: bridge (sign-in, refresh, other server, sign-out), slot refresh rules,
  REST retry, realtime reconnect after refresh (fails without the fix).

- A4-T09 — Launcher: invites client. `social/service/invites.rs`, `social/store/invites.rs`, desktop migration
  `0004_social_invites` (sender's join secret sealed with the chat key, accepted-here flag, join-sent flag, progress
  throttle), `ports::{Games, GameCheck, LaunchError, LocalLibrary, join_args}`. Commands `invites_list`,
  `invite_send`, `invite_accept`, `invite_decline`, `invite_cancel`; events `invite-received`, `invite-changed`,
  `invite-install-requested` (05-social-notes §5–§6, behaviour §3.2). Received `invite.join` with an invalid secret →
  normal launch (it was ignored before).
  Evidence: `tests/social_chat.rs::invite_install_ready_join_handshake` (the M3 demo on the real API, two launchers,
  fake library/launcher): missing game → install dialog event at once → `installing` progress → not `ready` before
  `InstallFinished` → `ready` → `invite.join` over Olm → launch with the secret → `joined`; installed game without a
  secret → normal launch; install failing its signature check → `failed/install_failed`, nothing launched; sender
  cancels → invitee sees `cancelled`, accepting then is a conflict; invalid secret refused before sending; the secret
  is in no server table. Store tests (secret sealed at rest and delivered over Olm, queued once, throttle every
  ≥ 5 s / 5 %, roles), `join_args` (whole-argument substitution, invalid → none), outcome mapping.
  **Not yet:** the real launch and update detection are Agent 2's (A2-T08/T09); until then `LocalLibrary` reads the
  `installs` table (installed = current) and cannot launch (`LaunchError::Unavailable`, logged).

## In progress
- A4-T10 — Overlay broker, macOS panel and fallbacks. **Part 1 done:** `vgames_overlay::protocol` (versioned
  length-prefixed postcard frames, closed enums, caps, constant-time token check, no-panic property test);
  launcher `overlay::{broker, hub, valve, commands}`: per-game loopback broker (random port, 32-byte token in the
  launch environment, one authenticated renderer at a time, listener closed after the handshake and re-bound for
  a reconnect, 5 failed handshakes stop it, 15 s dead-link timeout, bounded action queue), the view-model hub fed
  by social events (toasts 8 s, friends online, open invites, last messages; do not disturb), actions (accept /
  decline invite, quick reply, open launcher, close panel), Guide/PS hold toggles the panel, crash safety valve
  and per-package switches; commands `overlay_view`, `overlay_action` (overlay window) and
  `package_overlays_list`, `package_overlay_set` (Settings); events `overlay-view`, `overlay-package-disabled`.
  Tests: broker with a fake blocking renderer (views, panel, actions, only one renderer, wrong tokens → gives up
  after 5, garbage/oversized frames, invalid actions, reconnect on the same endpoint, stop), hub, valve.
  Hotkey: registered only while a game with the overlay runs, toggles the panel; `social_settings_set` checks a new
  one first (Ctrl/Alt/Super or an F-key; trial registration) → `SocialError` `invalid` / `in_use` (Agent 3's
  requested shapes, now generated: `HotkeyError` in `contract/settings.ts` can go).
  **Next:** always-on-top fallback
  window and OS notifications (Wayland), macOS `NSPanel` (needs a Mac to verify; `macOSPrivateApi` contract PR),
  wiring `prepare_launch` into Agent 2's launch plan once A2-T09 lands.

## Interfaces delivered (other agents may now rely on these)
- `vgames_desktop_lib::social::ports::SessionSlot` (A4-T07): now filled by `social::session_bridge` from
  `state.servers` (nothing for Agent 2 to call). Reach it through `State<SocialService>` → `sessions()`. Social commands for Agent 3: the friends/presence rows of
  05-social-notes §5 (`social_connection`, `social_settings_get/set`, `friends_list`, `friend_code_create`,
  `friend_request_send`, `friend_accept/decline/remove`, `user_block/unblock`, `blocks_list`, `user_profile`) and
  the four events above are in `bindings.ts`.
- **Agent 3 (A4-T08):** the messaging rows of 05-social-notes §5 are in `bindings.ts`: `conversations_list`,
  `conversation_open_direct`, `conversation_create_party`, `messages_list`, `message_send`, `message_retry`,
  `conversation_mark_read`, `typing_start`, `contact_security`, `contact_set_verified`, `contact_trust_device`,
  `devices_list`, `device_revoke`; events `conversations-changed`, `message-received`, `message-status-changed`,
  `typing`, `device-notice` (binding keys `conversationsChanged`, …, `deviceNotice`). The device OS type is
  `DevicePlatform` (the UI already has a `Platform`). Behaviour details: 05-social-notes §3.1.
- `vgames_proto::social::{DeviceList, DeviceKeysList, ClaimedKeyList, MAX_UNCLAIMED_ONE_TIME_KEYS, MAX_CLAIM_DEVICES}` (A4-T04).
- `vgames_proto::social` (A4-T02): social/messaging/invite DTOs, `canonical::{device_keys, one_time_key}` (the exact
  signed strings; server and launcher share them), `normalize_friend_code`, `is_valid_join_secret`.
  `vgames_proto::realtime::{kinds, PresenceChanged, FriendEvent, InboxNew, InviteEvent, DeviceEvent, Typing, TypingStart}`.
- **Agent 3:** build the Friends, Chat, Invites and Security screens against the commands and events in
  `05-social-notes.md` §5–§6 (names, argument keys and payloads are final; mock them in `src/mocks/backend.ts`).
  The overlay window's surface is §7.

## Needs from others
- From Agent 2 (A4-T10): in the launch plan (A2-T09), call `State<OverlayService>.prepare_launch(package, title)`
  before spawning and merge the returned variables into the game's environment (empty = overlay off). For the
  Windows DLL injection (A4-T11) I will need a "start suspended, run hook, resume" step in the launch plan.
- From Agent 3 (A4-T10): the Settings overlay commands are generated as `packageOverlaysList()` and
  `packageOverlaySet(packageRef, enabled)` (main-window commands may not start with `overlay_`, Agent 2's rule);
  I pointed your contract wrappers and mocks at them and removed the now-generated `PackageOverlay` type from
  `contract/settings.ts`. Show `overlay-package-disabled` as a notice with "Turn it back on".
- From Agent 2 (A4-T09): implement `social::ports::Games` over your library and launcher and install it with
  `State<SocialService>.set_games(Arc::new(…))`: `check(server, package)` → `Current | Missing | Outdated |
  NoBuildForPlatform` (your platform choice), `launch_join(server, package, secret)` → launch the manifest's
  `multiplayer.join.target` with `social::ports::join_args(&args, secret.as_deref())` (whole-argument substitution;
  `None` → normal launch). Publish `InstallProgress`/`InstallFinished` for installs the UI starts from
  `invite-install-requested` as for any install (the invite follows them). Failure codes containing `space`/`disk_full`
  map to `insufficient_space`.
- From Agent 2: launch-plan hook for overlay env/injection (A4-T10).
  Until they land, Agent 4 codes against small traits in `social::ports` and tests with in-process fakes.
- From Agent 3: a Vite entry for the overlay window (`apps/desktop/src/overlay/`, A4-T10).
- From Agent 5 (CI, A4-T08): `tests/social_chat.rs` (launchers against the real API) needs PostgreSQL next to
  WebKitGTK. Please add a `postgres:18` service to the desktop job and run
  `cargo test -p vgames-desktop --test social_chat -- --ignored` with `VGAMES_TEST_DATABASE_URL` set to its
  maintenance database (the test creates and drops its own database).
- From Agent 5 (CI): the desktop job only runs clippy. Please also run `cargo test -p vgames-desktop --locked`
  there (WebKitGTK is already installed in that job); the E2EE tests live in that crate.

- For Agent 2: `vgames-transfer` `tests/download.rs::protocol_violations_are_retried_once_on_a_fresh_connection`
  failed once locally under a full `cargo test` run (line 211) and passed 3/3 alone; it looks timing-sensitive.

## Cross-area edits (small, for the owners' review)
- Agent 2 (A4-T10): `lib.rs` `pub mod overlay;`; `names.rs` (`OVERLAY_WINDOW_COMMANDS = overlay_view,
  overlay_action`; two main-window commands), `capabilities/{main,overlay}.json`, `commands/mod.rs`, bindings; root
  `Cargo.toml` new workspace dependency `postcard` (add-only).
- Agent 3 (A4-T10): `src/ipc/contract/settings.ts` (removed `PackageOverlay`, now generated; wrappers call the
  renamed commands), `src/mocks/settings.ts` (handler names, `packageRef` argument).
- Agent 2 (A4-T09): `images.rs` `ImageKey::url()` (the `vgimg` URL for a key; used for invite covers);
  `db/migrations.rs` one `Migration { name: "0004_social_invites", … }` entry; commands/names/capabilities/bindings
  for the 5 invite commands and 3 events.
- Agent 2 (A4-T07 follow-up): none beyond `social::commands::init` reading `state.servers`/`state.bus` (my file);
  social REST still uses its own `reqwest` client with the bearer token from `Session::access_token`, not
  `ApiClient`: moving to `ApiClient` is possible once it exposes the problem body and a 401 hook.
- Agent 2 (A4-T08): `commands/mod.rs`, `commands/names.rs`, `capabilities/main.json` list the 13 messaging commands
  and 5 events; `bindings.ts` regenerated; `permissions/autogenerated/*` for them; `Cargo.toml` dev-dependencies
  `vgames-api` and `sqlx` (existing workspace dependencies; tests only).
- Agent 3 (A4-T07): `src/ipc/contract/settings.ts` imports the generated `SocialSettings` instead of declaring
  it (both exports collided in `src/ipc/index.ts`); the requested hotkey errors stay there for A4-T10.
- Agent 2 (A4-T07): `lib.rs` `setup()` calls `social::commands::init`; `commands/mod.rs` registers the social
  commands/events; `commands/names.rs` + `capabilities/main.json` list them; `bindings.ts` regenerated;
  `Cargo.toml`: `windows-sys` (Windows target, idle time) and `axum` (dev, fake gateway).
- Agent 1 (A4-T06): `packages.rs` `pub async fn summaries(state, ids)` (wraps the existing private helpers) for
  the package in invites.
- Agent 1 (A4-T03): `realtime/mod.rs` `start()` also starts `social::presence::start` (the presence heartbeat
  lives with the sockets); `realtime/hub.rs` `Hub::connected_users()`; `jobs/mod.rs` one `SCHEDULES` entry
  (`social.sweep`, 60 s, as `jobs/builtin.rs` anticipates).
- Agent 2: `apps/desktop/src-tauri/src/lib.rs` (`pub mod social;`), `db/migrations.rs` (the documented hook: one
  `Migration { name: "0002_social", … }` entry), `apps/desktop/src-tauri/Cargo.toml` (`chacha20poly1305`, `getrandom`,
  both existing workspace dependencies; `proptest` as a dev-dependency).

- Agent 5: E2EE code lives in `social/crypto/` (covered by the security-critical CODEOWNERS rule). `social/store.rs`
  also decrypts and encrypts at rest; consider adding `/apps/desktop/src-tauri/src/social/store*` to that section.

## Blockers / contract questions
- `contract:` `05-social-notes.md` v1.2 (A4-T09): new §3.2 (invite client behaviour). Additive.
- `contract:` `05-social-notes.md` v1.3 (A4-T10): §7 additions (overlay events, Settings commands, broker protocol,
  launch integration). Additive.
- `contract:` `05-social-notes.md` v1.1 (A4-T08): §2.1 fallback key ids prefixed `F`; new §3.1 (launcher messaging
  behaviour, typing throttle). Additive; no name or payload in §5–§6 changed.
- `contract:` `docs/architecture/05-social-notes.md` (new, A4-T01). Additive realtime changes in its §3
  (`presence.changed.package_title`, server → client `typing`) need a matching line in 03-api §6.
