---
audience: internal
component: launcher
type: security
---
`rosetta_install` now runs `softwareupdate --install-rosetta --agree-to-license` only after the player accepts a native OK/Cancel dialog shown by Rust, so a compromised WebView can no longer start it on its own (GAME-14 C4).
