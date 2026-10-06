---
audience: internal
component: launcher
type: fixed
---
Launcher e2e: the `chromium` Playwright project emulates reduced motion, so accessibility scans measure final colours instead of a dialog mid-fade (the screenshot lightbox caption failed contrast once during the 150 ms backdrop fade). The `perf` project is unchanged.
