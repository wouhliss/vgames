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

- A4-T05 — Server: conversations and message relay. ([PR #44](https://github.com/wouhliss/vgames/pull/44), merged) `apps/api/src/social/relay.rs`, migration
  `20260925130000_social_relay.sql` (`conversations.last_message_at`): direct get-or-create (friends, no block),
  parties (creator + 1–15 friends), activity-ordered list; sends need a device and membership, each envelope goes to
  an active device of a member or the sender (block either way → not addressable; a DM with a block → 404), ≤ 64 ×
  64 KiB (413), 120/min per device, idempotent per (sender device, client_message_id, recipient), `unknown_devices`;
  per-device inbox (oldest first, cursor), ack deletes only the caller's envelopes, `inbox.new` per recipient,
  expiry in `social.sweep`. `olm_message_type` now documents its `[0, 1]` enum in the generated OpenAPI.
  Evidence: 7 API tests (`tests/it/social_relay.rs`) incl. a real vodozemac session through the relay and a scan of
  every column of every table for the plaintext; mutation-checked (ack scope, membership, blocks).

- A4-T06 — Server: invite state machine. ([PR #44](https://github.com/wouhliss/vgames/pull/44), merged) `apps/api/src/social/invites.rs`, migration
  `20260925140000_social_invites.sql` (sender index; the one-active unique index already existed): create (accepted
  friends; strangers and blocks → 404; published package with a release; one active per triple returns it; 60/h),
  accept/decline (invitee), cancel (sender), status (installing/ready/joined/failed per 04-database §3; progress
  stored always, published ≤ every 2 s), every transition a guarded update (racing requests: one wins, the other
  409), expiry (pending 10 min, 24 h after accept) in `social.sweep` and lazily in its own transaction, events to
  both parties; blocks cancel live invites with `invite.updated`. All social operations of the contract are now
  implemented (`openapi_unimplemented.txt` is empty).
  Evidence: 7 API tests (`tests/it/social_invites.rs`): all 63 action × state cases, races, throttling over a real
  socket, expiry, create rules, rate limit; mutation-checked (state guard, role guard, throttle).

- A4-T07 — Launcher: realtime client, social state, presence (partly; see below). ([PR #44](https://github.com/wouhliss/vgames/pull/44), merged) `social::{ports, api, realtime,
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
  move to A4-T12. A `session.revoked` frame counts as the revocation even if the 4001 close is lost to a reset
  (seen on the Windows runner).
- A4-T08 — Launcher: messaging integration (partly; see below). `social::messaging` (part of `SocialService`): one
  work loop runs setup after every `hello` (Olm account, device registration or re-binding, OTK top-up below 20 up to
  50 plus the fallback key; a revoked device starts over with new keys and keeps history and blocks), conversation
  sync, the inbox (decrypt → store → ack; an unknown or changed sender device triggers a device refresh and one
  retry; unreadable envelopes are acknowledged and dropped), `device.added`/`device.revoked` (pins, notices), and the
  outbox (targets = members' non-revoked devices + own other devices, keys claimed and signature-checked for devices
  without a session, ≤ 64 envelopes per request, `unknown_devices` resent up to twice, retries with backoff while
  offline, a changed key fails the message until `contact_trust_device`). Message-store keys load from the keychain
  off the runtime (`SocialService::provide_keys`). Commands and events: the messaging rows of 05-social-notes
  §5–§6, including `typing_start` (throttled to one frame per 3 s per conversation) and `typing`. Server: realtime
  `typing` is relayed to the other members without a block either way (`relay::on_typing`). Evidence: 4 launcher
  tests through an in-memory relay with the API's rules over real sockets (two launchers chat; Bob's third device
  gets only new messages; revoking a device stops delivery to it; the outbox waits out an outage; a changed key
  blocks sending until trusted), 1 API test for typing (members only, blocks, non-members, bad data),
  mutation-checked. **Not yet:** the same scenarios against the real API with `VGAMES_PROFILE` launchers need
  signed-in sessions (A2-T07, reassigned); they move to A4-T12 with T07's.

## In progress
- A4-T09 — Launcher: invites client

## Interfaces delivered (other agents may now rely on these)
- `vgames_desktop_lib::social::ports::SessionSlot` (A4-T07, **for Agent 2's A2-T07**): call `set(Some(ServerSession
  { server_id, user_id, base_url, access_token }))` on sign-in, token refresh and server switch, `set(None)` on
  sign-out; register `on_unauthorized(|server_id| …)` to refresh a refused token. Reach it through
  `State<SocialService>` → `sessions()`. Social commands for Agent 3: the friends/presence rows of
  05-social-notes §5 (`social_connection`, `social_settings_get/set`, `friends_list`, `friend_code_create`,
  `friend_request_send`, `friend_accept/decline/remove`, `user_block/unblock`, `blocks_list`, `user_profile`) and
  the four events above are in `bindings.ts`. Messaging (A4-T08): `conversations_list`, `conversation_open_direct`,
  `conversation_create_party`, `messages_list`, `message_send`, `message_retry`, `conversation_mark_read`,
  `typing_start`, `contact_security`, `contact_set_verified`, `contact_trust_device`, `devices_list`,
  `device_revoke`, and the events `conversations-changed`, `message-received`, `message-status-changed`, `typing`,
  `device-notice`.
- `vgames_proto::social::{DeviceList, DeviceKeysList, ClaimedKeyList, MAX_UNCLAIMED_ONE_TIME_KEYS, MAX_CLAIM_DEVICES}` (A4-T04).
- `vgames_proto::social` (A4-T02): social/messaging/invite DTOs, `canonical::{device_keys, one_time_key}` (the exact
  signed strings; server and launcher share them), `normalize_friend_code`, `is_valid_join_secret`.
  `vgames_proto::realtime::{kinds, PresenceChanged, FriendEvent, InboxNew, InviteEvent, DeviceEvent, Typing, TypingStart}`.
- **Agent 3:** build the Friends, Chat, Invites and Security screens against the commands and events in
  `05-social-notes.md` §5–§6 (names, argument keys and payloads are final; mock them in `src/mocks/backend.ts`).
  The overlay window's surface is §7.

## Needs from others
- Seen (2026-09-26): Agent 5's two CI notes. GitHub Actions runs again since the repository went public;
  `scripts/ci/local.sh` stays optional and I run it before pushing.
- From Agent 2: API client with bearer tokens + refresh (A2-T07), active server/session lookup, library/install
  state and launch-with-args internal APIs (for invites, A4-T09), launch-plan hook for overlay env/injection (A4-T10).
  Until they land, Agent 4 codes against small traits in `social::ports` and tests with in-process fakes.
- From Agent 3: a Vite entry for the overlay window (`apps/desktop/src/overlay/`, A4-T10).
- From Agent 5 (CI): the desktop job only runs clippy. Please also run `cargo test -p vgames-desktop --locked`
  there (WebKitGTK is already installed in that job); the E2EE tests live in that crate.

- For Agent 2: `vgames-transfer` `tests/download.rs::protocol_violations_are_retried_once_on_a_fresh_connection`
  failed once locally under a full `cargo test` run (line 211) and passed 3/3 alone; it looks timing-sensitive.

## Cross-area edits (small, for the owners' review)
- Agent 3 (A4-T07): `src/ipc/contract/settings.ts` imports the generated `SocialSettings` instead of declaring
  it (both exports collided in `src/ipc/index.ts`); the requested hotkey errors stay there for A4-T10.
- Agent 2 (A4-T08): `commands/mod.rs`, `commands/names.rs`, `capabilities/main.json` list the messaging commands
  and events; `bindings.ts` regenerated.
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
- `contract:` `docs/architecture/05-social-notes.md` (new, A4-T01). Additive realtime changes in its §3
  (`presence.changed.package_title`, server → client `typing`) need a matching line in 03-api §6.
