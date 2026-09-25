# Agent 4 status

Networking & Multiplayer (`apps/api/src/social/**`, social migrations, `crates/vgames-proto/src/{social,realtime}.rs`,
the `social`/`messaging`/`invites` OpenAPI tags, `apps/desktop/src-tauri/src/{social,overlay}/**`,
`apps/desktop/src/overlay/**`, `crates/vgames-overlay/**`).

## Done
- A4-T01 — Adaptation note `docs/architecture/05-social-notes.md` (contract doc): Arachnel → vgames mapping,
  canonical JSON for device/OTK signatures, safety-number derivation, plaintext payload types, presence liveness,
  realtime events, and the full Tauri command/event surface for the social UI and the overlay window.

## In progress
- A4-T02 — E2EE core (launcher, offline-testable)

## Interfaces delivered (other agents may now rely on these)
- **Agent 3:** build the Friends, Chat, Invites and Security screens against the commands and events in
  `05-social-notes.md` §5–§6 (names, argument keys and payloads are final; mock them in `src/mocks/backend.ts`).
  The overlay window's surface is §7.

## Needs from others
- From Agent 2: API client with bearer tokens + refresh (A2-T07), active server/session lookup, library/install
  state and launch-with-args internal APIs (for invites, A4-T09), launch-plan hook for overlay env/injection (A4-T10).
  Until they land, Agent 4 codes against small traits in `social::ports` and tests with in-process fakes.
- From Agent 3: a Vite entry for the overlay window (`apps/desktop/src/overlay/`, A4-T10).

## Blockers / contract questions
- `contract:` `docs/architecture/05-social-notes.md` (new, A4-T01). Additive realtime changes in its §3
  (`presence.changed.package_title`, server → client `typing`) need a matching line in 03-api §6.
