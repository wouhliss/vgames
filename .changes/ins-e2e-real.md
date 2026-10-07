---
audience: internal
component: launcher
type: added
---
INS-09: `apps/desktop/e2e-real` drives the release launcher through tauri-driver and WebKitWebDriver under Xvfb against the real API behind `https://localhost` (throwaway CA, stunnel): M1 (add server, fingerprint, sign in, library, empty library and catalog) and M2 (browse, install, progress, play, stop, verify, uninstall). Nightly in `e2e.yml` job `launcher`.
