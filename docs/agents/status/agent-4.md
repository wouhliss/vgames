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
  changed-key blocking and trust, revoked devices dropped, no plaintext in the SQLite file.

## In progress
- A4-T03 — Server: friends, codes, blocks, profiles, presence

## Interfaces delivered (other agents may now rely on these)
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

## Cross-area edits (small, for the owners' review)
- Agent 2: `apps/desktop/src-tauri/src/lib.rs` (`pub mod social;`), `db/migrations.rs` (the documented hook: one
  `Migration { name: "0002_social", … }` entry), `apps/desktop/src-tauri/Cargo.toml` (`chacha20poly1305`, `getrandom`,
  both existing workspace dependencies).

## Blockers / contract questions
- `contract:` `docs/architecture/05-social-notes.md` (new, A4-T01). Additive realtime changes in its §3
  (`presence.changed.package_title`, server → client `typing`) need a matching line in 03-api §6.
