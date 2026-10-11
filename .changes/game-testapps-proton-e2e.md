---
audience: internal
component: launcher
type: added
---
GAME-05: `crates/vgames-testapps` (D3D11 and D3D12 Windows test games that draw a known colour, read it back and name the failing step in their exit code) and `compat-e2e.yml`, which runs the D3D11 game through real umu-launcher and UMU-Proton on hosted ubuntu-24.04 via `Launcher::launch`, proves a tampered Proton archive is refused before extraction and attempts D3D12 through VKD3D-Proton; the runtime catalog also accepts plain `tar` archives.
