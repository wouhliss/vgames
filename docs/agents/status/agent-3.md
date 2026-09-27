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

- A3-T04 — Library: virtualized grid/list (`@tanstack/react-virtual`, exact row heights, the shell's
  `<main>` scrolls), favorites pinned in their own section, collections as tabs (LB/RB switch them;
  drop a package on a tab to add it), search (case- and accent-insensitive), sort (recently played,
  name, size, install date; layout and sort remembered), badges (update / update recommended for a
  withdrawn version, incomplete, running, library offline, busy states, cloud-save state, Proton/Wine).
  Tile actions: Play (default target) / Stop (confirmed) / Resume / View download, launch options,
  favorite, add to collection (menu → dialog, or drag and drop), update (confirmation when the
  installed version was yanked), verify, move (library picker, offline and too-small libraries
  disabled), desktop shortcut, open folder, uninstall (leftover files and Proton/Wine prefix are kept
  unless ticked). Collections dialog: create, rename, reorder (buttons or drag and drop), delete.
  Every typed launch/action error has its own message; integrity and revoked-key errors offer
  "Verify files". Tests: 35 Vitest (model + page) and `e2e/library.spec.ts` (axe, keyboard-only,
  controller-only, performance). **5,000 packages:** 30 tiles in the DOM; 4 s continuous scroll =
  241 frames, p95 16.7 ms, max 16.8 ms, 0 dropped frames (> 34 ms), 0 long tasks. Memory test with
  the 40-package library: steady window +121–170 KB over 100 route cycles (< 256 KB), DOM nodes and
  listeners flat. Initial JS 117 KB gzipped; the Library chunk is 20 KB gzipped, loaded on demand.
  Shared fixes found by the browser tests: menus opened from the keyboard or a controller now take
  focus (they were `visibility: hidden` when focused), dialogs start focus in their body (not on
  Close), menu typeahead accepts spaces, and a failed background refresh keeps the last good list.

- Preview fixes (after the screenshot review): library focus rings are no longer clipped (`contain: paint`
  removed; the controller e2e test now fails if any ancestor clips the focused button), the Discord waiting
  screen no longer repeats its text, and the library toolbar fits a 960 px window.

- A3-T05 — Browse and package details ([PR #37](https://github.com/wouhliss/vgames/pull/37)). **Browse:** virtualized catalog grid over `<main>` (only nearby rows
  in the DOM; the next page loads as the last rows come into view, with a retry row when it fails),
  search debounced 300 ms (Escape clears), genre filter with counts, sort by name or recent updates
  (remembered), platforms per card (Windows · Mac · Linux), Installed and "Runs with Proton/Wine/Rosetta 2" /
  "Not available on this computer" badges, and empty, no-match (Clear filters) and error states.
  **Details:** hero, facts (version and size of the release for this computer, platforms, developer,
  publisher, release date, genres), SafeMarkdown description, screenshots with a viewer (arrows, D-pad,
  LB/RB, Ctrl+PageUp/Down; B returns focus to the thumbnail), compatibility (native / Rosetta 2 /
  Proton or Wine with status, admin notes as plain text and the ProtonDB tier as a hint), blockers
  ("DirectX 12 games need a Mac with Apple silicon" disables Install and says why; "Needs Rosetta 2"
  offers Install Rosetta 2), and Play plus the library actions when installed. **Install dialog:**
  version, platform, download size and space needed; libraries with free space (offline and too-small
  drives can't be picked); "None of your libraries has enough space" links to Storage settings;
  confirm → toast "X is queued" with View downloads. Every plan/start error has its own message
  (removed, no build for this computer, blocked, offline with Try again, not enough space, drive
  unplugged, trust expired). A package removed while its page is open: the dialog says so, and the
  page then shows "This package isn't available anymore" with focus on its title.
  Tests: 10 Vitest for Browse, 25 for details (all four acceptance cases, including the race versions:
  drive filled or unplugged after the dialog opened, release or package gone after the page loaded),
  3 for Tooltip; `e2e/browse.spec.ts` (axe on grid, details, install dialog and viewer; keyboard-only
  search → open → install; controller-only cards → details → screenshots → back; removed and
  unavailable packages).
  Shared fixes found on the way: after a navigation, focus moves to the new page's title (it was left
  on the body); the details page keeps one h1 through loading, content and removal; tooltips shift
  sideways to stay inside dialogs and panels; the router has a first-load fallback (no console warning).

- A3-T06 — Downloads ([PR #48](https://github.com/wouhliss/vgames/pull/48)). Three sections: **Downloading** (live from `install-progress`: phase, bytes, speed, time
  left, connections; indeterminate bar while verifying the signature, allocating and finishing),
  **Up next** (reorder with "Download next" / Move up / Move down in the row menu, or Alt+Up/Down; the move is
  announced and focus stays on the row) and **Completed** (outcome, size, when; Clear). Pause / Resume /
  Try again / Remove / Cancel per job; the cancel dialog keeps or deletes the partial files (no choice when
  nothing was downloaded; updates and repairs say the installed version stays). Every pause reason has its
  own message (disk full → "Open Storage settings"; drive disconnected; server unreachable) and every
  failure too: damaged server file ("vgames told the server's admins" when the integrity report was
  sent) with Try again; signature, untrusted key and expired trust show "Stopped to keep your computer
  safe" and **never offer a retry**; withdrawn version; I/O and server errors with Try again.
  Tests: 30 Vitest (each phase, each pause reason, each failure, cancel keep/delete/empty, reorder by menu
  and keyboard, history, action errors, load error; a mutation check confirms that offering a retry on a
  security failure fails 3 tests) and `e2e/downloads.spec.ts` (axe on the queue and the cancel dialog,
  live progress from the simulator, keyboard-only reorder/pause/cancel, controller-only).

## In progress
- A3-T07 — Settings, in parts. **Part 1 (this PR):** one URL per section (`/settings/<section>`, a section list that
  works with arrows and the D-pad) with General (theme incl. "Same as system", reduce motion), Servers (address,
  fingerprint, signed-in account, switch, remove, add through the first-run flow), Account (sessions with "sign
  out this device", sign out, warning when tokens are kept in a file instead of the keychain), Storage (free space,
  default library, add with the same path checks as onboarding, remove with "not empty" / "default" reasons, move
  every package of a library), Downloads (speed limit 0.1–1000 MB/s with validation, 1–3 installs at once),
  Updates (version, check now) and About (version, third-party licenses as plain text, copy diagnostics). Every
  setting is tested to survive a restart of the mock (disabling persistence fails 4 tests).
  **Next parts:** Privacy and Overlay (hotkey capture with conflict check, per-package overlay switch and safety
  valve), Compatibility, Controllers, Cloud saves (with A3-T08). What's new comes with A3-T10.

## Interfaces delivered (other agents may now rely on these)
- `apps/desktop/src/ipc/contract/`: the command/event surface the UI is built against, in exact
  tauri-specta output shape, one file per domain (`core.ts`: app, servers, auth, libraries;
  `library.ts`: installs, collections, favorites, launch, install actions). Each entry is deleted
  once the same name exists in `src/bindings.ts` (generated entries already win).
- `apps/desktop/src/ipc/contract/settings.ts`: account sessions, credential storage, library default/remove,
  download settings, social/overlay settings and licenses (see "Needs from others").
- `apps/desktop/src/ipc/contract/downloads.ts`: the install queue commands and `downloads-changed` (see
  "Needs from others").
- `apps/desktop/src/ipc/contract/catalog.ts`: catalog, package details, install plan/start and Rosetta 2
  commands the Browse and details screens use (see "Needs from others").
- **For Agent 4 (A4-T10):** `apps/desktop/overlay.html` is the overlay window's page; `vite.config.ts` adds it as
  a second build input (`dist/overlay.html`) as soon as `apps/desktop/src/overlay/main.tsx` exists. Point the
  overlay window's URL at `overlay.html`.
- Mock mode: `pnpm --filter @vgames/desktop dev:mock` runs the UI in a browser against
  `src/mocks/` (`?mock=fresh|ready|empty|huge|no-library|debug`; `ready` has 40 installed packages
  with one library offline, `huge` has 5,000). E2E: `pnpm --filter @vgames/desktop e2e`
  (builds the mock bundle; set `PLAYWRIGHT_CHROMIUM_EXECUTABLE` to use a preinstalled Chromium).

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
- **From Agent 2** (A2-T06/T08/T09/T10, for the Library, A3-T04). Full types and doc comments in
  `apps/desktop/src/ipc/contract/library.ts`:
  - `installs_list -> Result<Vec<InstalledPackage>, AppError>` (active server; includes incomplete
    installs; `state`: installed | incomplete | installing | updating | repairing | moving |
    uninstalling, where `incomplete` = not installed and no download job; `update` with
    `installed_yanked`; `favorite`, `collection_ids`, `running`, launch `targets`, `compat`
    native | proton | wine, `cloud_saves` unsupported | synced | syncing | pending | conflict,
    `cover_url` as a `vgimg:` URL).
  - Collections (global, items carry their server): `collections_list`, `collection_create(name)`,
    `collection_rename(collection_id, name)`, `collection_delete`, `collections_reorder(collection_ids)`,
    `collection_add_package(collection_id, package)`, `collection_remove_package` →
    `CollectionError { invalid_name | name_taken | not_found }`; `favorite_set(package, favorite)`.
  - `game_launch(package, target_id: Option) -> Result<(), LaunchError>` (not_installed, incomplete,
    busy, library_offline, already_running, target_not_found, integrity {path}, key_revoked,
    compat_unavailable {detail}, rate_limited, save_conflict {conflict_id}, io), `game_stop(package)`.
  - `install_update | install_verify | install_resume | install_move(package, library_id) |
    install_uninstall_plan -> UninstallPlan | install_uninstall(package, remove_leftovers,
    remove_prefix)` → `InstallActionError` (not_found, busy, running, library_offline, offline,
    insufficient_space {required_bytes, available_bytes}, same_library, io); `install_open_folder`,
    `shortcut_create -> path`.
  - Events: `installs-changed`, `collections-changed` (the UI also refreshes installs on
    `game-started`, `game-stopped` and `install-finished`).
- **From Agent 2** (A2-T08 install queue, 09-compatibility §1 release selection, for Browse and details,
  A3-T05). Full types and doc comments in `apps/desktop/src/ipc/contract/catalog.ts`:
  - `catalog_list(query: CatalogQuery {query, genre, sort, cursor}) -> Result<CatalogPage, CatalogError>`
    (proxies `GET /v1/packages`; each item carries `availability` for this computer:
    native | rosetta | proton | wine | unavailable, and `cover_url` as a `vgimg:` URL).
  - `catalog_genres -> Result<Vec<GenreCount {genre, count}>, CatalogError>` (see the contract question
    below).
  - `package_details(package_id) -> Result<PackageDetails, CatalogError>`: the details plus `release`
    (the build chosen for this computer per 09 §1: platform, version, size, `via`) and `compat`
    (`native` | `rosetta {blockers}` | `compat {layer, status, notes, protondb_tier, blockers}` |
    `unavailable`; blockers `needs_apple_silicon`, `needs_rosetta`, `rosetta_sunset {last_macos}`).
    `CatalogError`: not_found (unpublished or hidden), offline, unauthenticated, server {code}.
  - `install_plan(package_id) -> Result<InstallPlan {release, download_bytes, required_bytes},
    InstallPlanError>` (not_found, no_release, already_installed, offline, blocked {blocker},
    server {code}).
  - `install_start(package_id, library_id) -> Result<(), InstallStartError>` (the plan errors plus
    insufficient_space {required_bytes, available_bytes}, library_offline {library_path},
    trust_expired, io {detail}); emits `installs-changed` with the new `installing` entry.
  - `rosetta_install -> Result<(), AppError>` (macOS on Apple silicon; runs `softwareupdate`).
- **From Agent 2** (A2-T06/T08 install queue, 02-package-format §7, for Downloads, A3-T06). Full types and doc
  comments in `apps/desktop/src/ipc/contract/downloads.ts`:
  - `downloads_list -> Result<DownloadQueue {jobs, history}, AppError>`: jobs in queue order (running first),
    each with `kind` install | update | repair, `version_label`, `library_id`, last known
    `bytes_done/bytes_total`, and `state`: `active` | `queued` | `paused {reason}` | `failed {error}`.
    Pause reasons: `user`, `disk_full {library_path, required_bytes, available_bytes}`,
    `library_offline {library_path}`, `offline` (all but `user` clear by themselves). Failures:
    `damaged_file {reported}` (second hash mismatch; `reported` = integrity report sent), `signature_invalid`,
    `untrusted_key`, `trust_expired`, `version_unavailable`, `io {path, detail}`, `server {code, message}`.
    History: newest first, ≤ 100, with the generated `InstallOutcome`.
  - `download_pause | download_resume | download_retry | download_remove (package)`,
    `download_cancel(package, keep_partial)`, `downloads_reorder(packages)` (new order of the waiting jobs)
    → `DownloadActionError` (not_found, insufficient_space, library_offline, offline, io);
    `downloads_history_clear`.
  - Event `downloads-changed` (queue or history changed). Live progress keeps using the generated
    `install-progress`; `install-finished` ends a job.
- **From Agent 2** (Settings, A3-T07). Full types in `apps/desktop/src/ipc/contract/settings.ts`:
  - `account_sessions(server_id) -> Result<Vec<AccountSession>, AppError>` (`GET /v1/me/sessions`: id,
    device_name, platform windows | linux | macos | web | cli, created_at, last_used_at, current) and
    `account_session_revoke(server_id, session_id)`.
  - `credential_storage -> CredentialStorage` (`keychain` | `file_fallback {path}`, 01-security §7).
  - `library_set_default(library_id) -> Result<(), LibraryError>`;
    `library_remove(library_id) -> Result<(), LibraryRemoveError>` (LibraryError, `not_empty {install_count}`,
    `is_default`; files on disk are left alone).
  - `download_settings_get -> DownloadSettings {bandwidth_limit_kib: Option<u32> (≥ 100), concurrent_installs: 1..=3}`,
    `download_settings_set(settings) -> Result<DownloadSettings, SettingsError {invalid {field, detail} | io}>`.
  - `app_licenses -> Result<String, AppError>` (bundled third-party notices, plain text).
- **From Agent 4** (Settings → Privacy and Overlay, next part of A3-T07): `social_settings_get/set` as in
  05-social-notes §5, plus a typed error from `social_settings_set` when the overlay hotkey can't be registered:
  `invalid` (not an accelerator) or `in_use {by: Option<String>}` (the OS or another app holds it). Also
  `overlay_packages -> Vec<PackageOverlay {package, title, enabled, disabled_by_safety_valve_at}>` and
  `overlay_package_set(package, enabled)` (turning it on clears the safety valve).
- **From Agent 5:** nothing more for now. Seen (2026-09-26): CI runs locally (`scripts/ci/local.sh`, PR 42);
  my PRs carry its summary on the final commit and merge only when every job passes. The updater commands and the `updater-status` event now
  come from the generated `bindings.ts` (onboarding's "launcher too old" check uses `updater_check ->
  UpdateCheck`); A3-T10 builds the banner and What's new dialog on them.
- **From Agent 4:** the social UI (A3-T09) will be built from your A4-T01 note
  (`05-social-notes.md`). What the screens need, so the note can cover it: friends with presence
  ("Playing X", package title included), incoming/outgoing requests, friend codes with `expires_at`,
  remove/block; conversations list with unread counts and last message; a message list with delivery
  states (sending, sent, failed, received) and device-change notices; safety number + verified flag
  per contact; invites (send with optional join info; incoming invite event for the in-app card;
  accept/decline; the "install dialog opens now" event carrying the package; the sender's view of the
  invitee's progress and state; expiry/failure reasons); settings "show what I'm playing" and
  "do not disturb"; the per-package "In-game overlay" toggle, its safety-valve state and the overlay
  hotkey (with conflict check).
- **From Agent 1 / Agent 2** (contract question): when the Discord callback fails the registration
  policy (`registration_closed`, not on the allowlist, disabled user), how does the launcher learn it?
  Proposal: the API redirects to `vgames://auth/callback?error=<code>&client_state=…` and Rust emits
  `auth-finished {outcome: failed}`. The UI already handles each case.

## Blockers / contract questions
- None blocking.
- **For Agent 1 (genres):** Browse filters by genre (`GET /v1/packages?genre=`), but the API has no way
  to list the genres that exist. Proposal: `GET /v1/genres -> [{genre, count}]` over published packages
  visible to the caller (or a `genres` facet on the first `PackagePage`). Until it exists, `catalog_genres` has
  nothing to call, and the genre filter only offers "All genres".
- Note for Agent 1: the admin login shows `/admin/login?error=<code>` messages for
  `registration_closed`, `not_allowlisted`, `user_disabled`, `access_denied` if the web callback
  redirects there on failure.
