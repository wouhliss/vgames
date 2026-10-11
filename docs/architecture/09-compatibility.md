# 09 — Compatibility Layers: Windows Packages on Linux (Proton) and macOS (Wine)

Goal: an admin uploads **one Windows build**. Linux and Mac players install and play it with no
manual setup, and the launcher picks and maintains the right compatibility runtime. Native builds
always win when they exist.

**Version 1.1 (2026-10-05).** D3DMetal is deferred: no default chain includes it, no catalog entry, setting or
license screen ships, and D3D12 titles are refused up front on every Mac (§3). The launcher-owned environment
keys (§4), the per-key catalog version (§5), D3D detection before the full download (§3) and the prefix location
on every OS (§2, §3) are now normative.

## 1. Release selection

The launcher asks the server for the release matching its host, falling back to a Windows build:

| Host | Order of preference |
|---|---|
| Windows x86_64 | `windows-x86_64` |
| Windows arm64 | `windows-aarch64` → `windows-x86_64` (OS emulation) |
| Linux x86_64 | `linux-x86_64` → `windows-x86_64` via **Proton** |
| macOS arm64 | `macos-aarch64` → `macos-x86_64` (Rosetta 2) → `windows-x86_64` via **Wine** (+ Rosetta 2) |
| macOS x86_64 | `macos-x86_64` → `windows-x86_64` via **Wine** |

The UI shows a badge ("Runs with Proton", "Runs with Wine") and the package's compatibility status
(§4). Invites report `no_build_for_platform` only when neither a native build nor a compat path exists.
Installing a Windows build on Linux or macOS uses the normal download engine and the normal
signature checks (02-package-format). Only the *launch* differs.

## 2. Linux: Proton through umu-launcher

- **Runner:** [`umu-launcher`](https://github.com/Open-Wine-Components/umu-launcher) (`umu-run`), the
  unified Proton launcher used by Heroic, Lutris and Faugus. It runs Proton outside Steam inside the
  Steam Linux Runtime container (pressure-vessel), as Steam does, and applies community
  **protonfixes** by `GAMEID`. It is GPL-3.0 and invoked as a separate process (no linking).
- **Proton builds:** UMU-Proton (Valve Proton + umu patches, the default) or GE-Proton
  ([`proton-ge-custom`](https://github.com/GloriousEggroll/proton-ge-custom), which publishes `.sha512sum` files).
  Versions and hashes are pinned by the runtime catalog (§5); the launcher passes `PROTONPATH` to
  the verified copy, so umu never downloads an unpinned Proton on its own.
- **Steam Linux Runtime:** downloaded and maintained by umu from Valve into `~/.local/share/umu`.
  This is the one component outside our pinning (trust boundary: umu + Valve over HTTPS), **accepted**
  (decision 2026-09-24, §7).
- **Invocation** (built by the launcher, explicit argv, no shell):
  `umu-run <install>/<launch.executable> <args…>` with `WINEPREFIX=<prefix>` (§2.1),
  `GAMEID` (§4), `STORE=none` (or `steam` when the umu id comes from a Steam app id),
  `PROTONPATH=<runtime dir>`, profile `env`, and `WINEDLLOVERRIDES` from the compat profile.
- **Windows dependencies** (`vcrun2022`, `d3dcompiler_47`, …): an allowlisted list of winetricks verbs
  in the compat profile, installed once per prefix with `umu-run winetricks <verb>` (winetricks checks
  its own download hashes).
- **Checks before first launch:** Vulkan driver present (query with `ash`), 32-bit Vulkan loader where needed,
  free space for the prefix (~1 GB), unprivileged user namespaces available (needed by pressure-vessel).
  Every failure has a specific message and fix hint.
- **Controllers:** Proton maps controllers to XInput itself (SDL/hidraw). vgames' virtual-pad layer
  stays **off** for Proton launches (07-controllers §3).

### 2.1 Prefixes (every OS)

One Wine/Proton prefix per package and server: `<data_dir>/prefixes/<server_id>/<package_id>`, where
`<data_dir>` is the launcher's `AppPaths::data_dir` (Tauri's app data directory; on macOS
`~/Library/Application Support/<bundle identifier>`, with no special case). It is computed by one pure function,
`compat::prefix_dir(data_dir, package)`, created on first compat launch, and removed with the install when the
player asks for it.

## 3. macOS: Wine with a Metal graphics backend

- **Wine:** WineHQ macOS builds ([`Gcenx/macOS_Wine_builds`](https://github.com/Gcenx/macOS_Wine_builds),
  LGPL, x86_64 binaries running under **Rosetta 2** on Apple Silicon). Pinned by hash in the runtime catalog.
- **Graphics backends**, tried in the compat profile's order (default below):

| Backend | APIs | License / distribution | Notes |
|---|---|---|---|
| **DXMT** ([`3Shain/dxmt`](https://github.com/3Shain/dxmt)) | D3D10/11 → Metal | LGPL-2.1, redistributable | Preferred open-source DX11 path |
| **DXVK-macOS + MoltenVK** | D3D10/11 → Vulkan → Metal | zlib + Apache-2.0 | Fallback for DXMT gaps |
| wined3d (built into Wine) | D3D ≤ 9, OpenGL | LGPL | Old titles |

  Default chain on every Mac: **DXMT → DXVK-macOS + MoltenVK → wined3d**.
- **D3DMetal (Apple Game Porting Toolkit) is deferred in v1** (§7). No D3DMetal code path, catalog entry,
  setting or license screen ships. The formats keep accepting it as data: `d3dmetal` stays a valid
  `runner.graphics` value (`vgames_core::compat::Graphics::D3dmetal`) and runtime id
  (`vgames_core::runtimes::RuntimeId::D3dmetal`). A profile that lists it is honoured from its next backend on,
  and a local override cannot select it (`graphics_unavailable`). Bringing it back is a data change: a catalog
  entry for the host architecture (`redistribution = "non-commercial"`, shipped unmodified and complete with
  Apple's license text, offered only on Apple-branded Macs) enables it, and the license then appears in
  Settings → Compatibility → Licenses.
- **D3D detection, before the full download.** The launcher decides which D3D a title needs before the pack
  bytes arrive:
  1. a compat-profile hint decides before any byte is downloaded;
  2. otherwise the install fetches the launch executable first (the download engine's priority-files option)
     and a bounded scan of its PE import table (`d3d9`, `d3d10`, `d3d11`, `d3d12.dll`) decides before
     downloading the remaining installation data. Chunks needed for the executable can also contain
     bytes from other files.

  The result is recorded per (server, package, version), so details and retries show it without downloading
  again. **D3D12 on any Mac** is refused with the blocker `d3d12_unsupported_on_mac` ("needs DirectX 12, not
  available on Mac yet") until the runtime catalog lists D3DMetal for the host architecture; it is never a
  crash at launch.
- **Rosetta 2:** detected at startup (`arch -x86_64 /usr/bin/true`); if missing, prompt to install it
  (`softwareupdate --install-rosetta` behind an explicit confirmation). Apple announced that macOS 27 is
  the last release with full Rosetta 2, and that from macOS 28 only a subset for older games remains. The
  launcher must track Apple's policy per macOS release and surface "may stop working on macOS 28+" in
  the compat status. **Native Apple Silicon builds are the long-term answer**, and the admin UI should
  encourage uploading them.
- **Prefixes:** as on Linux (§2.1), under Tauri's app data directory. Retina/high-DPI and `RetinaMode` defaults
  in the compat profile.
- **Controllers:** Wine maps controllers to XInput itself (IOHID/SDL backends). Native Mac games use Apple's
  Game Controller framework, which supports Xbox, PlayStation (DS4/DualSense), Nintendo Switch Pro/Joy-Con and MFi
  controllers. See 07-controllers §4 for true virtual pads.

## 4. Compat profiles (signed, updatable without re-uploading the package)

Compatibility settings change execution, so they are **signed**. Compat tweaks often arrive after a
release, so they live in a separate small document instead of the manifest:

```json
{
  "format": "vgames.compat/1",
  "server_id": "…", "package_id": "…",
  "target": "linux",                      // linux | macos
  "revision": 3,                          // strictly increasing per (package, target)
  "created_at": "2026-09-24T10:00:00Z",
  "applies_to": { "platform": "windows-x86_64", "min_sequence": 1, "max_sequence": null },
  "status": "verified",                   // verified | playable | unsupported | untested
  "notes": "Controller works out of the box.",
  "runner": {
    "kind": "proton",                     // proton (linux) | wine (macos)
    "prefer": ["umu-proton", "ge-proton"],
    "min_version": "GE-Proton10-1",
    "umu_game_id": "umu-12345",           // optional; enables protonfixes
    "graphics": ["dxmt", "dxvk", "wined3d"],   // macos only; "d3dmetal" accepted, skipped in v1 (§3)
    "env": { "PROTON_ENABLE_NVAPI": "1" },
    "dll_overrides": { "d3dcompiler_47": "native,builtin" },
    "winetricks": ["vcrun2022"]
  }
}
```

- Signed with a publisher key (01-security §3), context `vgames/compat/v1`. The launcher verifies it
  against the same trust state as manifests and refuses a lower `revision` than the one it has.
- Validation (`vgames-core::compat`): env keys `^[A-Z_][A-Z0-9_]{0,63}$` minus the manifest env denylist and
  minus the launcher-owned keys below; DLL override names `^[a-z0-9_.-]{1,64}$`; winetricks verbs from
  `WINETRICKS_ALLOWLIST` in `vgames-core`.
- **Launcher-owned environment keys** (`vgames_core::compat::is_launcher_owned_env_key`): the launcher sets these
  itself for compat launches, and they never come from a profile, a manifest or a local override:
  `WINEPREFIX`, `WINEDLLOVERRIDES`, `WINEDLLPATH`, `WINEPATH`, `WINELOADER`, `WINESERVER`, `PROTONPATH`, `GAMEID`,
  `STORE`, and every key starting with `STEAM_COMPAT_`, `UMU_` or `PRESSURE_VESSEL_`. DLL overrides reach Wine
  only through the validated `dll_overrides` map.
- No profile → defaults (`status: untested`; UMU-Proton on Linux; DXMT → DXVK-macOS + MoltenVK → wined3d on
  every Mac).
- **Hints for admins:** when a package has a Steam app id, the metadata job also fetches the ProtonDB
  summary tier (informational only, shown in the admin UI) and looks up the umu id in the umu-database.
- **Local overrides:** Settings → Compatibility lets a player override runtime version, graphics
  backend and extra env per package. Overrides are stored locally, marked as such in diagnostics, and
  resettable.

## 5. Runtime catalog (who is allowed to put runtime binaries on a machine)

Proton, umu, Wine, DXMT, DXVK-macOS and MoltenVK are third-party code the launcher downloads and executes.
They follow the same rule as packages: **nothing runs unless its bytes are pinned by a signature we trust.**

- `runtimes.json` (`vgames.runtimes/1`) lists every runtime version: id, version, OS, architecture,
  upstream URL, **SHA-256**, size, license, and minimum launcher version. It is signed with the **vgames
  runtime-catalog key** (minisign/Ed25519, separate from the updater key; public key compiled into the launcher).
- It is published as an asset of the project's GitHub release stream and refreshed by a scheduled
  workflow maintained by Agent 5, which checks upstream releases, downloads, hashes, smoke-tests and opens a PR.
  Signing happens in the protected `release` environment.
- **Rollback protection per key (F2).** The launcher keeps the highest catalog version it accepted **per
  catalog key** (the minisign key id), never one global number: a high version signed by one key never blocks a
  catalog signed by another (a rotated key starts its own sequence). A lower version under the same key is
  refused. Release builds trust only the compiled-in key; a test override of the URL and key exists only in
  debug builds.
- The launcher refreshes the catalog on startup, on a server switch and before a compat launch, at most once
  every 6 h (a stored timestamp, never a timer).
- The launcher downloads runtimes into `<app data>/runtimes/<id>/<version>/`, verifies the size while
  downloading and the SHA-256 **before** extracting, extracts with the same path-safety rules as packages (no
  `..`, no absolute paths, no link leaving the target, no device files), keeps the versions still
  referenced by an installed package, and garbage-collects the rest.
- Upstream checksum files (for example GE-Proton `.sha512sum`) are cross-checked by the catalog workflow but
  never trusted on their own at runtime.

## 6. Cloud saves under a compatibility layer

`saves.locations` bases resolve **inside the prefix** for compat launches (06-cloud-saves §1). Examples:
`documents` → `<prefix>/drive_c/users/steamuser/Documents` (Proton) or
`<prefix>/drive_c/users/<mac user>/Documents` (Wine); `appdata` → `…/AppData/Roaming`. A save
snapshot records the **Windows-relative** path, so saves move freely between Windows, Linux and macOS players.

## 7. Decisions (recorded 2026-09-24)

| Decision | Outcome | Consequence |
|---|---|---|
| Is vgames distributed commercially? | **No: vgames is non-commercial** | D3DMetal could be redistributed through the runtime catalog under Apple's license (§3). If vgames ever becomes commercial, no `non-commercial` entry may stay in the catalog |
| Steam Linux Runtime trust boundary | **Accepted**: umu downloads it from Valve over HTTPS, unpinned | We do not mirror it. umu-launcher itself and Proton stay pinned |
| D3DMetal in v1 (owner, 2026-10-05) | **Deferred** | No D3DMetal code path, catalog entry, setting or license screen ships. Macs run D3D9–11 through Wine + DXMT, DXVK-macOS + MoltenVK or wined3d; D3D12 titles get `d3d12_unsupported_on_mac` up front on every Mac until the catalog lists D3DMetal for the host architecture (a later data change). Formats keep accepting `d3dmetal` |
