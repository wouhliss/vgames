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

## In progress
- A3-T02 — App shell
- A3-T03 — Onboarding and server selection
- A3-T13 — Admin foundation

## Interfaces delivered (other agents may now rely on these)
- `apps/desktop/src/ipc/contract.ts`: the command/event surface the UI is built against, in exact
  tauri-specta output shape. It is replaced by re-exporting `src/bindings.ts` once that exists.
- Mock mode: `pnpm --filter @vgames/desktop dev:mock` runs the UI in a browser against
  `src/mocks/backend.ts` (`?mock=fresh|ready|no-library|debug`).

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
    `auth_cancel(flow_id)`, `libraries_list`, `library_pick_folder -> Result<Option<FolderPick>, AppError>`
    (native dialog from Rust), `library_add(path, make_default) -> Result<Library, LibraryError>`.
  - Events: `ui-nav {action, controller, repeat}` (controller intents already debounced and
    auto-repeated in Rust; the UI never polls gamepads), `active-controller-changed`,
    `connectivity-changed {server_id, online}`, `trust-problem` (fingerprint mismatch),
    `server-add-requested {url, fingerprint}` (from `vgames://server/add`), `auth-finished`,
    `servers-changed`, `libraries-changed`.
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
- **From Agent 1 / Agent 2** (contract question): when the Discord callback fails the registration
  policy (`registration_closed`, not on the allowlist, disabled user), how does the launcher learn it?
  Proposal: the API redirects to `vgames://auth/callback?error=<code>&client_state=…` and Rust emits
  `auth-finished {outcome: failed}`. The UI already handles each case.

## Blockers / contract questions
- None blocking.
