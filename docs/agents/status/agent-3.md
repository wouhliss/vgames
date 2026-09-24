# Agent 3 status

Frontend UX/UI (`apps/desktop/src/**` except `overlay/` and `bindings.ts`, `apps/admin-web/**`,
`packages/api-client/**`).

## Done

## In progress
- A3-T01 — Launcher foundation (tokens, components, spatial navigation, typed IPC layer, i18n, gallery)
- A3-T13 — Admin foundation (generated client, fetch wrapper, error handling, layout, login)

## Interfaces delivered (other agents may now rely on these)

## Needs from others
- From Agent 2: `bindings.ts` (A2-T01). Until it lands, the launcher UI codes against a hand-written
  stand-in with the same shape as tauri-specta output; the exact command and event list I need will be
  posted here with A3-T01.

## Blockers / contract questions
