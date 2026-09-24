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
