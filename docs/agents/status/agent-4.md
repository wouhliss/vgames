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

## In progress
- A4-T05 — Server: conversations and message relay

## Interfaces delivered (other agents may now rely on these)
- `vgames_proto::social::{DeviceList, DeviceKeysList, ClaimedKeyList, MAX_UNCLAIMED_ONE_TIME_KEYS, MAX_CLAIM_DEVICES}` (A4-T04).
- `vgames_proto::social` (A4-T02): social/messaging/invite DTOs, `canonical::{device_keys, one_time_key}` (the exact
  signed strings; server and launcher share them), `normalize_friend_code`, `is_valid_join_secret`.
  `vgames_proto::realtime::{kinds, PresenceChanged, FriendEvent, InboxNew, InviteEvent, DeviceEvent, Typing, TypingStart}`.
- **Agent 3:** build the Friends, Chat, Invites and Security screens against the commands and events in
  `05-social-notes.md` §5–§6 (names, argument keys and payloads are final; mock them in `src/mocks/backend.ts`).
  The overlay window's surface is §7.

## Needs from others
- From Agent 2: API client with bearer tokens + refresh (A2-T07), active server/session lookup, library/install
  state and launch-with-args internal APIs (for invites, A4-T09), launch-plan hook for overlay env/injection (A4-T10).
  Until they land, Agent 4 codes against small traits in `social::ports` and tests with in-process fakes.
- From Agent 3: a Vite entry for the overlay window (`apps/desktop/src/overlay/`, A4-T10).
- From Agent 5 (CI): the desktop job only runs clippy. Please also run `cargo test -p vgames-desktop --locked`
  there (WebKitGTK is already installed in that job); the E2EE tests live in that crate.

- For Agent 2: `vgames-transfer` `tests/download.rs::protocol_violations_are_retried_once_on_a_fresh_connection`
  failed once locally under a full `cargo test` run (line 211) and passed 3/3 alone; it looks timing-sensitive.

## Cross-area edits (small, for the owners' review)
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
