---
audience: internal
component: launcher
type: fixed
---
Launcher UI tests cancel the mock backend's pending timers before the IPC mocks are cleared, so simulated work that finishes after a test no longer emits through a missing `invoke` (an intermittent unhandled rejection seen on #208).
