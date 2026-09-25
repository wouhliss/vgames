---
audience: internal
component: launcher
type: added
---
Library UI (A3-T04) against a requested command contract: installs, collections, favorites, launch, update/verify/move/uninstall and shortcut commands plus installs-changed and collections-changed events (apps/desktop/src/ipc/contract/library.ts). The pending contract is split per domain (the updater now comes from the generated bindings), the mock backend is split per domain with 5,000-package fixtures, and PLAYWRIGHT_CHROMIUM_EXECUTABLE lets the e2e suites use a preinstalled Chromium.
