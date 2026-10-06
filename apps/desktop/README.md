# vgames desktop launcher

Tauri 2 app. The Rust core lives in `src-tauri/` (Agent 2; `social/` and `overlay/` are Agent 4's,
`updater/` is Agent 5's). The React UI lives in `src/` (Agent 3). The UI talks to the core only through
the generated `src/bindings.ts` (tauri-specta).

## Build dependencies

**Linux** (Ubuntu 24.04 / Debian 12 package names):

```sh
sudo apt install build-essential pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev \
  libsoup-3.0-dev libjavascriptcoregtk-4.1-dev librsvg2-dev libxdo-dev \
  libayatana-appindicator3-dev libssl-dev libudev-dev libdbus-1-dev
```

Fedora: `webkit2gtk4.1-devel gtk3-devel libsoup3-devel librsvg2-devel libxdo-devel
libappindicator-gtk3-devel openssl-devel systemd-devel dbus-devel`. Arch: `webkit2gtk-4.1 gtk3 libsoup3
librsvg xdotool libappindicator-gtk3 openssl systemd dbus`.

**Windows:** Visual Studio Build Tools (C++ workload) and WebView2 (preinstalled on Windows 11).
**macOS:** Xcode Command Line Tools.

## Run

```sh
pnpm install
pnpm dev:desktop                         # Vite on :1420 + the Rust core in debug mode
VGAMES_PROFILE=alice pnpm dev:desktop    # debug only: separate data dir, logs and single-instance lock
VGAMES_LOG=debug pnpm dev:desktop        # tracing EnvFilter directive
```

The main window starts hidden and appears when the UI calls `commands.appReady()`
(or after 15 s if the UI never does, so a broken UI cannot leave an invisible app).

## Bindings

Debug builds rewrite `src/bindings.ts` at startup when commands or events changed. Without running the app:

```sh
VGAMES_UPDATE_BINDINGS=1 cargo test -p vgames-desktop bindings
```

`cargo test -p vgames-desktop` fails when the committed file is stale, when `src-tauri/src/commands/names.rs`
misses a command, or when `capabilities/*.json` grant the wrong commands. The overlay window may only call
`overlay_*` commands (enforced by Tauri permissions generated in `build.rs`).

Adding a command: implement it with `#[tauri::command] #[specta::specta]`, add it to `collect_commands!`
in `src-tauri/src/commands/mod.rs` and to `names.rs`, and grant `allow-<name-with-dashes>` in the capability
of the window that needs it.

## Logs and crash reports

| OS | Directory |
|---|---|
| Linux | `~/.local/share/app.vgames.launcher/logs` |
| Windows | `%LOCALAPPDATA%\app.vgames.launcher\logs` |
| macOS | `~/Library/Logs/app.vgames.launcher` |

`vgames.log` rotates at 10 MB and keeps 7 files. Tokens, bearer credentials, URL query strings
and secret-looking `key=value` pairs are redacted. A panic writes `crash-<time>-<pid>.txt`
next to the logs. Nothing is uploaded. Profiles use `app.vgames.launcher.profile-<name>`.

## Performance scripts

`perf/idle.py <pid> [seconds]` (Linux) reports CPU time, PSS and context switches per process over a window,
for the launcher and its WebKit processes.

## Troubleshooting

- **`pkg-config` cannot find `webkit2gtk-4.1`:** install the Linux build dependencies above.
- **The second launch does nothing:** that is expected. It forwards its arguments to the running instance and
  exits. Use a different `VGAMES_PROFILE` in debug builds to run two launchers.
- **Blank window under WSLg or in VMs:** try `WEBKIT_DISABLE_DMABUF_RENDERER=1`.

## Keyboard and controller walkthrough (INT-10)

Run `pnpm --filter @vgames/desktop e2e`. Use Tab/Shift+Tab and Enter/Space on the keyboard;
use D-pad/left stick and A to activate, B to close a dialog with a controller. Tests emit the typed
`ui-nav` actions; physical-pad detection is PLAY-04. Focus must remain visible and return to the
opener after every dialog. `e2e/helpers.ts::axeScan` rejects serious and critical findings. The route
coverage test reads the production router, so a new screen needs a scan case.

| Route or dialog | Keyboard and controller steps | Automated evidence |
|---|---|---|
| `/onboarding` | Enter address, confirm fingerprint, sign in, choose storage; cancel and retry each step. | `onboarding.spec.ts` full first-run keyboard/controller cases and shared scans |
| `/library` | Search, change view, select a package, play and open its menu. | `library.spec.ts` keyboard/controller and grid/list scans |
| Collections and package collections | Open from the library menu, select a collection, close to the opener. | `library.spec.ts` collection keyboard case and dialog scan |
| `/browse` | Search, move between cards, open package details and return. | `browse.spec.ts` keyboard/controller cases |
| `/package/:packageId` | Reach Install/Play and screenshots; open and close the install and screenshot dialogs. | `browse.spec.ts` details/install/screenshots scans and controller case |
| Install, Rosetta confirmation and screenshots | Choose storage with arrows, confirm with Enter/A, cancel with Escape/B. | `browse.spec.ts` and `PackagePage.test.tsx` Rosetta cases |
| `/friends/*` | Switch Friends/Requests/Chats, add a code and send a message. | `friends.spec.ts` screen/dialog scans and keyboard cases |
| Add friend, invite, safety number and invite install | Activate the dialog action, review details, confirm/cancel and restore focus. | `friends.spec.ts` and `FriendsPage.test.tsx` |
| `/downloads` | Select a job, pause/resume, reorder and confirm cancellation. | `downloads.spec.ts` keyboard/controller and queue/menu/dialog scans |
| `/settings/*` | Move between every section; tab to its controls and toggle a setting. | `settings.spec.ts` every-section scan and keyboard/controller cases |
| Server removal, session sign-out, library removal/move | Review the named confirmation, cancel or confirm and restore focus. | `settings.spec.ts` keyboard/controller confirmation scans and focus return; `SettingsPage.test.tsx` mutation cases |
| How a game runs, compatibility licenses and third-party licenses | Open, read/scroll, change allowed options and close. | `settings.spec.ts` game options keyboard scan; `settings.spec.ts` third-party license keyboard/controller scans; settings units cover runtime licenses |
| What's new | Open the update banner, scroll notes, choose Later or Install and restart. | `update.spec.ts` keyboard/controller and long-changelog scans |

`routes.spec.ts` scans every production screen. `memory.spec.ts` measures navigation after warmup;
`library.perf.spec.ts` measures 5,000 items independently. Initial production scripts and modulepreloads
must total at most 250,000 gzipped bytes, checked by `scripts/ci/bundle-size.mjs` in CI and local checks.
Real-application navigation awaits INS-09's harness; the same scan helper is available to it.
