---
audience: internal
component: launcher
type: added
---
Launcher release pipeline: desktop-v* tags build a draft release in the approval-gated release environment with OS-signed installers, updater artifacts signed for exactly their version (cargo xtask updater sign/manifest/verify), latest.json, the agent-written player changelog, SBOMs, checksums and provenance; a desktop build matrix on Windows, Linux and macOS; actionlint in CI.
