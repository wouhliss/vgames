---
audience: internal
component: launcher
type: added
---
PLAY-06 UI acceptance: Playwright `e2e/saves.spec.ts` runs the cloud-save conflict dialog (opened from the refused launch and from the event) and Settings → Cloud saves history/restore with axe, keyboard-only and controller-only input; the mock backend persists save conflicts and histories across a restart, and a restore records its own backup.
