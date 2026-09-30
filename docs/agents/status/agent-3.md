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

- A3-T10 — Update banner and What's new, on Agent 5's generated updater commands. A banner under the top bar
  (non-blocking): "vgames X is available" → What's new / Later (Later hides that version for the session; a newer one
  shows again), then download progress, then "Restarting…"; an install that failed is reported until dismissed, a
  failed background check stays quiet. **What's new:** every release in `(installed, new]`, grouped New / Improved /
  Fixed / Removed / Security, **plain text only**; a release with no entries is the single line "Stability and
  performance improvements."; `latest.json` notes (fallback) as a plain list; the notes are a focusable scrolling
  region, so long changelogs scroll with the keyboard or D-pad. **Install and restart** is disabled with its reason
  while a game runs or while installing; while downloads are active it explains they pause first. Settings → Updates
  opens the same dialog. Tests: 19 Vitest (incl. the zero-entries and long-changelog cases, markup shown as text) and
  `e2e/update.spec.ts` (axe on banner and dialog, keyboard-only install, Escape/B return focus, long changelog scroll).

- A3-T09 — Friends, chat and invites (05-social, 05-social-notes §5–6). `/friends` with tabs (LB/RB) that are URLs:
  **Friends** grouped Playing / Online / Offline ("Playing X", or "Playing a game" when hidden; presence live from
  `presence-changed`), Message, Invite to play, and a menu with Safety number, Remove (confirmed) and Block (confirmed);
  **Add friend**: your code (single use, 15 min countdown, Copy, "expired → create a new code") and a friend's code
  (Crockford normalization: lower case, O→0, I/L→1, dashes and spaces ignored; sent once 8 characters are there; every
  `SocialError` has its sentence: code doesn't work, rate limited with the wait, limits, already friends, offline);
  **Requests** (accept, decline, block; cancel outgoing); **Blocked** (unblock); a notice while the social socket
  reconnects. **Messages** (`/friends/messages/<id>`): conversation list with unread counts, a virtualized history
  (`role="log"`, focusable, follows new messages, "Load earlier messages"), plain text only, device-change notices,
  delivery states (Sending…, Sent, Not sent + Try again), "… is typing" for 5 s, composer with the 4,000-character limit
  (Enter sends, Shift+Enter new line); a key change pauses sending with "Review safety number". **Safety number:** 12
  groups of 5, "Mark as verified", the contact's devices, "Trust new key" behind a confirmation, and "messages don't move
  to new devices". **Invites:** "Invite to play" from a friend (pick an installed game) or a package page (pick a
  friend), optional message (200) and join info (checked with 05-social §5's pattern, sent end-to-end encrypted);
  **incoming invite cards** stacked under the top bar (Accept / Decline / Decide later; a card whose invite expired or
  was cancelled says so); `invite-install-requested` opens the install dialog at once; the sender sees each state
  (incl. the invitee's install progress bar and every failure reason) and can cancel while it makes sense; the invitee
  sees theirs. Tests: 37 Vitest (every invite state for both sides, card accept/decline/later/expired/cancelled,
  4,000 limit, key change, send/retry, typing, codes) and `e2e/friends.spec.ts` (axe on friends, requests, messages,
  add friend, safety number, invite card and the install dialog it opens; keyboard-only and controller-only card →
  Accept → install; keyboard-only add friend and send message). Memory test with Friends in the loop: steady window
  204 KB (< 256 KB), nodes and listeners flat. Found on the way: a query that fails refetches on every visit, so the
  mock now answers `social_connection` (a missing handler cost ~400 KB per 100 visits).

- A3-T08 — Cloud save UX (06-cloud-saves §3), against `src/ipc/contract/saves.ts` (see "Needs from others"). **Conflict
  dialog:** both sides side by side (time, device, file count, size), three choices (keep the cloud saves / keep this
  device / keep both) and Cancel; nothing is preselected and Continue stays off until a choice is made, so it never
  resolves on its own; Cancel, Escape and B change nothing and the game doesn't start; it opens from Play
  (`save_conflict`), from a `save-sync` conflict after the game exited, and from Settings; errors (game running, offline,
  already resolved, I/O) stay in the dialog, and "the cloud changed again" shows the newer details and clears the
  choice. **Notices** for the other outcomes of the decision table: restored before launch, uploaded after exit,
  pending (server unreachable; the tile badge already says "Saves not synced"), failed. **Settings → Cloud saves:**
  games with cloud saves and their state, "Resolve the conflict", per-game history (server snapshots, local backups,
  the current head can't be restored over itself) and Restore behind a confirmation that mentions the backup; offline
  hides snapshots but keeps backups. Tests: 32 Vitest (`routes/saves/saves.test.tsx`: every outcome, each choice, each
  error) and `e2e/saves.spec.ts` (axe on the dialog, list and history; keyboard-only and controller-only).

- A3-T14 — Admin packages list and editor. **List** (`/admin/packages`): status and text filters kept in the URL
  (back/forward and deep links work), table with sticky headers, cursor "Load more", empty / no-match / error (retry)
  / forbidden states, "Deleted X." after a delete. **Create**: title (required, 1–200 code points), optional slug with a
  live preview of the server's `slugify`, Steam/IGDB ids, "look up metadata"; one `Idempotency-Key` per form plus a
  synchronous guard, so a double click or a retry after a lost answer creates one package; server field errors and
  `slug_taken` land on the fields. **Editor**: every field with its source badge (admin / IGDB / Steam) and a counter,
  client limits mirroring the API (code points after trimming: title 200, summary 500, description 20,000, developer
  and publisher 200, ≤ 20 genres of ≤ 64, ids ≥ 1, slug pattern, date), focus to the first invalid field; saves send
  only what changed as `application/merge-patch+json` with `If-Match`; **412** keeps the input and shows a
  field-by-field diff (theirs / yours) with "keep mine, then save" (only my differences go over their version) or
  "load theirs"; server `genres[i]` errors map onto Genres; dirty guard on in-app navigation (Stay / Leave) and tab
  close; status change with a confirmation that says what it does; delete with the slug typed. Tests: 29 Vitest and
  `e2e/packages.spec.ts` (axe on list, create, editor; double click = one package; concurrent edit 412 with diff, via
  the mock server or, against the real stack, a second page; every field at 0 / max / max+1 with RTL, emoji and accents;
  keyboard-only filter → open). The mock server now keeps its data across reloads in the tab (a `?mock=` URL starts over).

- A3-T15 — Metadata review and images. The package page now has tabs (Details, Metadata, Images; own URLs) that share
  the cached package and its ETag. **Metadata:** the lookup job's state in words, polled every 2 s while queued or
  running and stopped at a terminal state (tested: no request after it ends); a failed/dead job shows its error and
  attempts with "Try again"; candidates (source, title, year, score); a side-by-side comparison (current / candidate)
  with a checkbox per field (differing values ticked by default, admin-edited fields marked and kept unless "Overwrite
  fields an admin edited" is on, which warns); apply with `If-Match`, and a 412 asks to reload, then apply. Candidate
  image URLs are shown as text (the admin page loads no third-party images). **Images:** cover, hero, logo and
  screenshots from `/v1/assets/{id}` with a placeholder when one can't be loaded; upload checked before sending (content
  sniffed as JPEG/PNG/WebP, 1 byte – 10 MiB), with progress (XHR: fetch has no upload progress), then a new
  cover/hero/logo is set with `If-Match`; the server's 413/415 shown; delete with confirmation. Tests: 17 Vitest
  (`metadata.test.tsx`, `api/upload.test.ts` with a fake XHR: CSRF, progress, 413/415, malformed, schema, network,
  timeout, abort) and `e2e/metadata.spec.ts` (axe on both tabs, failed job + retry, 412 on apply, too big / wrong type
  refused locally, a real multipart upload through the service worker, broken image, delete dialog).

- A3-T16 (part 1: versions and browser upload). **Versions tab:** state, sequence, label, platform, size and file
  count, creator, current-release badge, verification progress (polled every 2 s while one verifies); publish
  (confirmed), yank (reason 3–500 required), abort (confirmed), continue upload. **Upload wizard**
  (`/packages/<id>/versions/new`, `…/versions/<vid>/upload` to resume): platform + label (`Idempotency-Key` per form) →
  folder (`<input webkitdirectory>`, or the File System Access API when present, which also finds empty folders) with
  every invalid path listed and its reason (02 §3 rules checked in TypeScript for the preview, again by pack-wasm), plan
  summary and a virtualized file list → the executable to start (+ arguments, or "nothing to start") → key file +
  passphrase, unlocked **only inside a dedicated key worker** that offers nothing but `sign(digest)` and is terminated
  after finalize (a root key is refused) → upload. **Engine:** a pack worker reads and hashes chunks and streams pack
  bytes from any offset; the page PUTs 16 MiB pieces to GCS resumable sessions, 4 packs in parallel; session URIs and
  confirmed offsets go to IndexedDB after every piece; on resume the re-picked folder must match (paths, sizes, mtimes)
  or it's refused with the differences; network loss waits for `online`/backoff and resumes by itself; an expired start
  URL gets a new one; a vanished session restarts that pack; 5xx backs off; `beforeunload` and in-app navigation ask
  while uploading; one tab per upload (Web Locks, BroadcastChannel fallback). Then the manifest is built, signed, PUT,
  finalized (every 422 code has a human explanation; "use another key file" for key problems; `pack_missing` re-checks
  the packs), verification is followed, and Publish. Tests: 34 new Vitest (engine against MSW + an emulated GCS: full
  run, pause/resume without resending, network loss, expired start URLs, 503s, resume after "reload", untrusted and
  someone else's key, a changed file, failed verification; path rules; versions tab; wizard end to end, invalid paths,
  root key, changed folder on resume) and `e2e/upload.spec.ts` in Chromium with real Web Workers: **a network cut and a
  page reload mid-upload, then resume and complete**, wrong passphrase, key not in the trust bundle (422
  `publisher_key_untrusted`), **a 100,000-file folder** (planned in the worker, < 100 rows in the DOM, input stays
  responsive). The 2 GB run against the real API + fs storage is for the nightly e2e workflow (not run here).
- A3-T16 (part 2: Compatibility tab). One editor per target, Linux (Proton) and macOS (Wine): status, notes (plain text,
  2,000), Windows build and version range, preferred runtimes, minimum runtime version, umu id (Linux; the umu id from
  the Steam candidate and the ProtonDB tier are shown as hints), graphics backends in order (macOS), env and DLL
  overrides as lines, winetricks from the `vgames-core` allowlist. Every `vgames-core::compat` rule is checked before
  signing (env key syntax and the launcher-owned/denied names like `LD_PRELOAD`, `STEAM_COMPAT_*`; DLL names and load
  orders; runtime ids; duplicates). Saving builds the `vgames.compat/1` document with the next revision, hashes it in the
  pack worker, signs it in the key worker (`vgames/compat/v1`, the same unlocked publisher key flow as uploads) and PUTs
  it; 409 (newer revision published meanwhile) and 422 (untrusted key, invalid profile) are explained. The current
  revision is shown read-only. Tests: 5 Vitest + an e2e sign-and-publish in the real key worker, axe on both tabs.
- A3-T17 — Users, allowlist, settings, trust, jobs, audit log. **Users:** search (username or Discord id) and role
  filter in the URL, cursor paging, status with the disable reason, last seen; disable (reason ≤ 500) / enable; admins
  can't act on admins or owners (the button says "Owners manage admins"); owners change roles from a select with a
  confirmation that says what the role allows; `cannot_disable_self` and `last_owner` explained; a 403 is handled
  even when a control was shown. **Allowlist:** Discord id checked (5–25 digits, with where to find it), note ≤ 200,
  `already_allowlisted` on the field, remove with confirmation. **Settings:** owners edit name, message of the day and
  registration mode with `If-Match` (a 412 keeps the input and offers keep-mine or load-theirs); admins see them
  read-only. **Trust:** bundle version and expiry, publisher keys with holder and validity, "Expires in N days" under
  60 days, expired and revoked (with reason) marked; owners upload a bundle + signature, every refusal explained
  (`bad_signature`, `wrong_server`, `invalid_bundle`, `stale_version`, `unknown_holder`). **Jobs:** state and kind
  filters, attempts, last error as text, retry for failed/dead (`job_already_queued` explained). **Audit log:**
  actor/action/target/date-range filters in the URL (actor checked as a UUID), cursor paging, details as JSON text.
  Tests: 17 Vitest and `e2e/authorization.spec.ts`: **the admin vs owner matrix for every page and action**
  (owner-only controls present for owners, absent for admins; axe on every page for both roles) plus a forced
  owner-only request answered 403.

## In progress
- A3-T07 — Settings, in parts. **Part 1** ([#52](https://github.com/wouhliss/vgames/pull/52), merged): one URL per section (`/settings/<section>`, a section list that
  works with arrows and the D-pad) with General (theme incl. "Same as system", reduce motion), Servers (address,
  fingerprint, signed-in account, switch, remove, add through the first-run flow), Account (sessions with "sign
  out this device", sign out, warning when tokens are kept in a file instead of the keychain), Storage (free space,
  default library, add with the same path checks as onboarding, remove with "not empty" / "default" reasons, move
  every package of a library), Downloads (speed limit 0.1–1000 MB/s with validation, 1–3 installs at once),
  Updates (version, check now) and About (version, third-party licenses as plain text, copy diagnostics). Every
  setting is tested to survive a restart of the mock (disabling persistence fails 4 tests).
  **Part 2** ([#62](https://github.com/wouhliss/vgames/pull/62), merged): Privacy (show what I'm playing, do not disturb) and Overlay (on/off; the shortcut is recorded by
  pressing it: keys named by position, a plain key or Shift + key refused locally, the Rust core's `invalid` and
  `in_use {by}` shown; Escape cancels; Reset to Shift+F3; a switch per game, with the date and reason when the
  crash safety valve turned it off). Tested to persist through a restart. Also fixes a unit-test teardown race
  (`clearMocks()` before a late `listen` settled) that made about half of the PackagePage runs report an unhandled error.
  **Part 3 (this PR):** Compatibility (09-compatibility): how Windows games run here (Proton, Wine, or natively on Windows);
  Rosetta 2 on Apple silicon (install after confirming, and Apple's "limited after macOS 27" note from the runtime
  catalog); the default Proton/Wine version (automatic or a catalog version); downloaded runtimes with their size,
  what uses them and Remove for unused ones; per-game overrides (runner version, graphics backend on Macs with
  D3DMetal only on Apple silicon, extra `NAME=value` environment lines checked locally and by the core; saving the
  defaults removes the override; Reset); runtime licenses as plain text, including Apple's for D3DMetal. The
  default and the overrides are tested to persist through a restart. Also: select lists and menus opened inside a
  dialog now render in the dialog's layer (they were drawn under it and couldn't be clicked).
  **Next part:** Controllers (the Cloud saves section is done with A3-T08).

## Interfaces delivered (other agents may now rely on these)
- `apps/desktop/src/ipc/contract/`: the command/event surface the UI is built against, in exact
  tauri-specta output shape, one file per domain (`core.ts`: app, servers, auth, libraries;
  `library.ts`: installs, collections, favorites, launch, install actions). Each entry is deleted
  once the same name exists in `src/bindings.ts` (generated entries already win).
- `apps/desktop/src/ipc/contract/settings.ts`: account sessions, credential storage, library default/remove,
  download settings, social/overlay settings and licenses (see "Needs from others").
- `apps/desktop/src/ipc/contract/compat.ts`: runtimes, the default runner, per-package overrides and runtime
  licenses for Settings → Compatibility (see "Needs from others").
- `apps/desktop/src/ipc/contract/saves.ts`: `saves_conflict`, `saves_resolve`, `saves_history`, `saves_restore` and the
  `save-sync` / `saves-changed` events (see "Needs from others").
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
- **From Agent 2** (A2-T11 cloud saves, 06-cloud-saves §3, for A3-T08). Full types and doc comments in
  `apps/desktop/src/ipc/contract/saves.ts`: `saves_conflict(conflict_id) -> SaveConflict {local, cloud: SaveSide
  {changed_at, device_name, file_count, size_bytes}}`, `saves_resolve(conflict_id, choice: keep_cloud | keep_device |
  keep_both) -> SaveResolved {launched}` (`SaveConflictError`: not_found, running, offline, head_moved {conflict}, io),
  `saves_history(package) -> {snapshots, backups}`, `saves_restore(package, source)`, and the events `save-sync
  {package, title, outcome: restored | uploaded | pending | conflict {conflict_id} | failed}` and `saves-changed`.
  `game_launch` already returns `save_conflict {conflict_id}`; after a choice that came from Play the core should start the
  game itself (`launched: true`). Cancelling sends nothing.
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
- **From Agent 2** (A2-T16/T17 runtime settings, for Settings → Compatibility, A3-T07). Full types in
  `apps/desktop/src/ipc/contract/compat.ts`:
  - `compat_overview -> Result<CompatOverview, AppError>`: `host` (`native` on Windows | `proton` |
    `wine {apple_silicon, rosetta: installed | missing | null, rosetta_last_macos}` from the catalog's Rosetta
    field), `runtimes` on disk (`{runtime, version, size_bytes, used_by}`), `runners` from the catalog for this
    computer (newest first), `graphics` this Mac can use (D3DMetal only on Apple silicon), `default_runner`
    (null = automatic).
  - `compat_default_set(runner: Option<RunnerVersion>) -> Result<(), CompatSettingsError>`;
    `runtime_remove(runtime, version) -> Result<(), RuntimeRemoveError {not_found | in_use {used_by} | io}>`.
  - `compat_packages -> Result<Vec<PackageCompat {package, title, layer, override}>, AppError>` (installed
    packages that launch through Proton/Wine); `compat_override_set(package, CompatOverride {runner, graphics,
    env}) -> Result<(), CompatSettingsError>` (`not_found`, `unknown_runner`, `graphics_unavailable {backend}`,
    `invalid_env {key, reason: format | denied}` per `vgames-core::compat` and the manifest env denylist, `io`);
    `compat_override_reset(package)`.
  - `compat_licenses -> Result<Vec<RuntimeLicense {runtime, name, spdx, text}>, AppError>` (the licenses shipped
    next to the host's runtimes; Apple's for D3DMetal on Macs).
- **From Agent 4** (Settings → Privacy and Overlay, A3-T07): `social_settings_get|set` are generated now and used
  as is. Still requested (A4-T10): `SocialError` variants for an overlay hotkey that can't be registered,
  `invalid` (not an accelerator) and `in_use {by: Option<String>}` (the OS or another app holds it); until then an
  `invalid_input {field: "overlay_hotkey"}` shows as "not a shortcut vgames can use". Also
  `overlay_packages -> Vec<PackageOverlay {package, title, enabled, disabled_by_safety_valve_at}>` and
  `overlay_package_set(package, enabled)` (turning it on clears the safety valve).
- **From Agent 5** (A3-T16): the admin release build must run `pnpm --filter @vgames/pack-wasm build` before
  `pnpm --filter @vgames/admin-web build`; without it the uploader says its packing module is missing. The admin pages
  bundle the `.wasm` (≈ 250 KB gzipped) through an optional glob, so CI builds without it keep working.
- **From Agent 5:** nothing more for now. Seen (2026-09-26): CI runs locally (`scripts/ci/local.sh`, PR 42);
  my PRs carry its summary on the final commit and merge only when every job passes. The updater commands and the `updater-status` event now
  come from the generated `bindings.ts` (onboarding's "launcher too old" check uses `updater_check ->
  UpdateCheck`); A3-T10 builds the banner and What's new dialog on them.
- **From Agent 4** (A3-T09, built): the messaging commands and events of A4-T08 (types copied from your branch into
  `apps/desktop/src/ipc/contract/social.ts`; delete them there when they land in `bindings.ts`) and the invite
  commands `invites_list`, `invite_send`, `invite_accept|decline|cancel` with the events `invite-received`,
  `invite-changed`, `invite-install-requested` exactly as 05-social-notes §5–6.
- **From Agent 4 (history):** the social UI (A3-T09) was built from your A4-T01 note
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
- **For Agent 1 (compat history):** A3-T16 asks for a read-only revision history per compat target, but the API only
  serves the latest profile per target (`GET /v1/packages/{id}/compat`, and only for published packages). Proposal:
  `GET /v1/admin/packages/{id}/compat/{target}` → all revisions (newest first). Until then the tab shows the current
  revision only.
- **For Agent 2 (library commands, A2-T08):** the generated `library_remove` returns `LibraryRemovalError` without
  the requested `is_default`. Can the default library be removed? Settings → Storage offers Remove on it and says
  "make another one the default first" only if the core refuses with `is_default`. My pending library entries in
  `src/ipc/contract/{core,settings}.ts` are superseded by the generated ones; I'll switch the UI to `LibraryInfo`
  / `LibraryActionError` / `LibraryRemovalError` and delete them in a follow-up.
- **For Agent 1 (genres):** Browse filters by genre (`GET /v1/packages?genre=`), but the API has no way
  to list the genres that exist. Proposal: `GET /v1/genres -> [{genre, count}]` over published packages
  visible to the caller (or a `genres` facet on the first `PackagePage`). Until it exists, `catalog_genres` has
  nothing to call, and the genre filter only offers "All genres".
- Note for Agent 1: the admin login shows `/admin/login?error=<code>` messages for
  `registration_closed`, `not_allowlisted`, `user_disabled`, `access_denied` if the web callback
  redirects there on failure.
