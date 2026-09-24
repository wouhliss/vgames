# Agent 3 status

Frontend UX/UI (`apps/desktop/src/**` except `overlay/` and `bindings.ts`, `apps/admin-web/**`,
`packages/api-client/**`).

## Done
- A3-T01 — Launcher foundation: tokens (dark / light / high contrast, reduced motion), components
  (Button, IconButton, TextField, TextArea, Select, Checkbox, Switch, Dialog, ConfirmDialog (typed),
  Menu, ContextMenu, Toasts, ProgressBar, Tabs, Tooltip, Card, Badge, Empty/Error/Loading states,
  SafeMarkdown, controller Glyphs), spatial navigation (`src/nav`), typed IPC + TanStack Query layer
  (`src/ipc`), i18n (`src/i18n`), `/dev/gallery` (dev builds only). 50 Vitest tests.
  Initial JS: 118 KB gzipped (budget 250 KB).

- A3-T02 — App shell: sidebar + top bar (server switcher, account menu, download indicator, update
  banner slot), lazy routes, `app_ready` after first paint, global error boundary with "copy
  diagnostics", offline banner (`connectivity-changed`), non-dismissible trust block (`trust-problem`).
  Memory test (`e2e/memory.spec.ts`, Chromium + CDP): 5 routes × 100 cycles after warm-up —
  heap 4.38 → 5.13 MB during the first 100 cycles (one-time settling), then +90 KB over the next 100
  (assertion: < 256 KB); DOM nodes 327 → 327, JS listeners 321 → 321.
- A3-T03 — Onboarding: address → fingerprint check (large grouped fingerprint + explainer) → Discord
  sign-in (waiting, open browser again, paste code) → library folder (free space) → done. Every edge
  case has its own message and a Playwright test (`e2e/onboarding.spec.ts`, 23 tests incl. full
  keyboard-only and controller-only runs). Initial JS 126 KB gzipped.

- A3-T13 — Admin foundation: `packages/api-client` (openapi-typescript + openapi-fetch; the generator
  pins TypeScript 5 locally because it needs the TS 5 compiler API), fetch wrapper
  (`apps/admin-web/src/api/http.ts`: same-origin credentials, `X-CSRF-Token` from `__Host-vgames_csrf`,
  30 s timeout, problem+json → typed `ApiError`, zod validation of every body with compile-time checks
  against the generated types), query policy (no 4xx retry, 2 retries with backoff for network/5xx,
  429 waits for `Retry-After`), 401 → sign-in with `return_to`, 403/404 pages, offline banner, login,
  zero-animation CSS. MSW handlers shared by Vitest (34 tests) and Playwright (5 tests incl. axe and
  a no-animation check).

## In progress
- A3-T04 — Library

## Interfaces delivered (other agents may now rely on these)
- `apps/desktop/src/ipc/contract.ts`: the command/event surface the UI is built against, in exact
  tauri-specta output shape. It is replaced by re-exporting `src/bindings.ts` once that exists.
- Mock mode: `pnpm --filter @vgames/desktop dev:mock` runs the UI in a browser against
  `src/mocks/backend.ts` (`?mock=fresh|ready|no-library|debug`). E2E: `pnpm --filter @vgames/desktop e2e`
  (builds the mock bundle, needs `playwright install chromium`). **For Agent 5 (CI):** please run it in
  `ci.yml` next to the unit tests.

## Needs from others
- **From Agent 2** (`bindings.ts`, A2-T01/T07/T08/T12). Please implement these names and shapes
  (full types and doc comments in `apps/desktop/src/ipc/contract.ts`; Rust enums as
  `#[serde(tag = "kind", rename_all = "snake_case")]`). If you prefer other names, tell me and I adapt.
  - Commands: `app_ready`, `app_info -> AppInfo`, `app_diagnostics -> String` (redacted),
    `open_external_url(url) -> Result<(), AppError>` (http/https only), `appearance_get`,
    `appearance_set(settings)`, `servers_list`, `server_preview(url, expected_fingerprint: Option)`
    `-> Result<ServerPreview, ServerError>`, `server_confirm(preview_id)`, `server_switch(server_id)`,
    `server_remove(server_id)`, `auth_start(server_id) -> Result<AuthFlow, AuthError>`,
    `auth_open_browser(flow_id)`, `auth_submit_code(flow_id, code) -> Result<Account, AuthError>`,
    `auth_cancel(flow_id)`, `auth_sign_out(server_id)`, `libraries_list`, `library_pick_folder -> Result<Option<FolderPick>, AppError>`
    (native dialog from Rust), `library_add(path, make_default) -> Result<Library, LibraryError>`.
  - Events: `ui-nav {action, controller, repeat}` (controller intents already debounced and
    auto-repeated in Rust; the UI never polls gamepads), `active-controller-changed`,
    `connectivity-changed {server_id, online}`, `trust-problem` (fingerprint mismatch),
    `server-add-requested {url, fingerprint}` (from `vgames://server/add`), `auth-finished`,
    `servers-changed`, `libraries-changed`.
  - `AuthFlow` carries `browser_opened: bool` (false → the UI offers the paste-code fallback at once).
  - **Error model:** `bindings.ts` uses one `CommandError { code: ErrorCode, message }` for every
    command. That is enough for most commands, but onboarding and libraries need to tell the user
    exactly what went wrong, with data (e.g. `launcher_too_old {min_version, current_version}`,
    `fingerprint_mismatch {expected, actual}`, `tls`, `not_vgames`, `nested_in_library {library_path}`).
    Request: the commands above return their specific enums (`ServerError`, `AuthError`,
    `LibraryError` in `contract.ts`). Alternative that also works for me: add an optional
    `detail` tagged union to `CommandError`.
  - `AppInfo` from `bindings.ts` is used as is (the UI reads `debug_build`). The keychain-fallback flag
    (01-security §7) is still needed; proposed as `keychain_fallback: bool` on the account/session
    command for Settings → Account (A3-T07).
  - FYI: `pnpm lint` fails on `main` because of `apps/desktop/src-tauri/icons/icon-source.svg`
    (Biome `noSvgWithoutTitle`); it is in your area.
- **From Agent 5:** `updater_check() -> UpdateCheck` (used by the "launcher too old" onboarding prompt),
  besides `updater_status` / `updater_whats_new` / `updater_install` for A3-T10.
- **From Agent 1 / Agent 2** (contract question): when the Discord callback fails the registration
  policy (`registration_closed`, not on the allowlist, disabled user), how does the launcher learn it?
  Proposal: the API redirects to `vgames://auth/callback?error=<code>&client_state=…` and Rust emits
  `auth-finished {outcome: failed}`. The UI already handles each case.

## Blockers / contract questions
- None blocking.
- Note for Agent 1: the admin login shows `/admin/login?error=<code>` messages for
  `registration_closed`, `not_allowlisted`, `user_disabled`, `access_denied` if the web callback
  redirects there on failure.
