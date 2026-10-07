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

## Installs

An install lives in a library folder as `<library>/<slug>/` (the package's slug, made safe for the
file system, with a suffix when that folder is taken). Next to the game's files,
`.vgames/` holds what the launcher needs to trust and repair it later:

| File | What it is |
|---|---|
| `install.json` | The install record (`vgames.install/1`): server, package, version, platform, state |
| `manifest.json`, `manifest.sig` | The signed manifest the files were checked against, and its signature |
| `journal.bin` | During an install or update only: which chunks are already on disk (02-package-format §7.9) |

- **States** (`installs_list`): `installed`, `installing`, `updating`, `repairing`, `moving`,
  `uninstalling` and `incomplete` (an install with no queued job, to resume or remove). Only `installed` can be
  launched; every launch first re-checks the signature under the server's current trust bundle.
- **Update** (`install_update`) fetches only the chunks that changed and swaps them in with a
  journaled commit, so an interruption at any point resumes or rolls back on the next start. A
  release older than the installed one is refused unless the installed version was withdrawn.
- **Verify files** (`install_verify`) hashes every file against the stored manifest and re-fetches
  the chunks of damaged ones. If the server rotated or revoked its publisher key, it adopts the
  server's current signature when it names the same version and covers the same manifest bytes
  (security review F1); nothing is downloaded for that.
- **Move** (`install_move`) renames within one drive, or copies every file, verifies the copy and
  only then deletes the source.
- **Uninstall** (`install_uninstall`) removes the files the manifest lists. Files the package did
  not ship (mods, configs, local saves) and the Proton/Wine prefix
  (`<data dir>/prefixes/<server>/<package>`) are only removed when the player ticks them. Links
  are never followed.

## Downloads

The queue (`downloads_list`, Downloads page) runs one to three installs at a time
(`download_settings_set`), in queue order, with an optional bandwidth limit applied live.

- **Order:** the launch target's files first (02 §7.6), then the rest.
- **Pause, cancel, reorder** act at the next chunk boundary. Cancelling keeps or deletes what was
  downloaded, as the player chooses.
- **Crash or power loss:** written chunks are fsynced and recorded in `journal.bin` every
  2 seconds; the next start re-queues the job and fetches only the chunks the journal does not
  have. Data on disk never exceeds the final size; only the journal comes on top.
- **Paused by the launcher** (with the reason shown): the library folder is gone (a drive
  unplugged) or the disk is full. Focusing the window re-checks.
- **Launcher updates** wait until every download has paused at a checkpoint, then restart; the
  queue resumes from the journals.
- **Refused before anything is written:** a manifest whose signature does not verify under the
  server's trust bundle (`untrusted_key`, `signature_invalid`, `revoked_key`, …). The launcher
  fetches the bundle once more before refusing, so a server that set up signing after it was
  added still works.
- **Damaged on the server:** a chunk whose hash does not match is fetched again on a fresh
  connection; a second mismatch fails the install with "A file on the server is damaged"
  (`damaged_file`) and sends an **integrity report** (below).

## Publishing (admins)

Admins and owners of the active server see **Publish** in the sidebar (`src/routes/publish/`, commands
`publish_*` and `version_yank` in `src-tauri/src/publishing/`). Every command checks the role again.

- **Before anything is sent:** the folder is scanned and every entry that can't be packed is listed
  (symbolic links, names Windows refuses, case collisions, …); the key file is decrypted in Rust and
  checked against the server's trust bundle (known, not revoked, held by this account, valid now).
- **Upload:** packs upload in parallel and resume where they stopped. The key is dropped once the
  manifest is signed; the server then checks every chunk and the version waits, verified, until
  **Release**. **Withdraw** (yank) needs a reason.
- **Stop, restart, resume:** a job lives in `<app data>/publishing/<job id>/` (`job.json`, never a key,
  plus the upload resume record). After a restart it shows as stopped; resuming asks for the key
  again only if the manifest is not signed yet.
- **Tests:** `src/publishing/tests.rs` (no upload), `tests/publish_e2e.rs` against the real API
  (`VGAMES_PUBLISH_E2E_GIB`, 2 on PRs and 5 nightly), `src/routes/publish/PublishPage.test.tsx` and
  `e2e/publish.spec.ts` (mock fixtures in `src/mocks/publishing.ts`).

## Logs and crash reports

| OS | Directory |
|---|---|
| Linux | `~/.local/share/app.vgames.launcher/logs` |
| Windows | `%LOCALAPPDATA%\app.vgames.launcher\logs` |
| macOS | `~/Library/Logs/app.vgames.launcher` |

`vgames.log` rotates at 10 MB and keeps 7 files. Tokens, bearer credentials, URL query strings
and secret-looking `key=value` pairs are redacted. A panic writes `crash-<time>-<pid>.txt`
next to the logs. Nothing is uploaded. Profiles use `app.vgames.launcher.profile-<name>`.

**Installs and downloads in the log.** The queue logs under `vgames_desktop_lib::downloads` and the
transfer engine under `vgames_transfer`: pauses with their reason, failures with their code
(`damaged_file`, `untrusted_key`, `insufficient_space`, …), repairs ("repairing damaged files" with
a count) and F1 signature adoption ("adopted the re-signed manifest signature"). Package and
version ids are logged; signed URLs and tokens never are.

**Reading an integrity report.** When a chunk fails its hash twice, the launcher posts
`POST /v1/versions/{version_id}/integrity-reports` with the pack index, the chunk index and a short
detail; it carries no file content. The server records it per player; when three different players
report the same pack within 24 hours it re-verifies that pack (`pack.reverify` job) and tells the
admins the outcome. So one report usually means a bad connection or a local problem, while
reports from several players mean the stored pack is damaged and must be re-uploaded.

## Performance scripts

`perf/idle.py <pid> [seconds]` (Linux) reports CPU time, PSS and context switches per process over a window,
for the launcher and its WebKit processes.

## Troubleshooting

- **`pkg-config` cannot find `webkit2gtk-4.1`:** install the Linux build dependencies above.
- **The second launch does nothing:** that is expected. It forwards its arguments to the running instance and
  exits. Use a different `VGAMES_PROFILE` in debug builds to run two launchers.
- **Blank window under WSLg or in VMs:** try `WEBKIT_DISABLE_DMABUF_RENDERER=1`.
