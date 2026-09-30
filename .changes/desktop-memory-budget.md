---
audience: internal
component: launcher
type: fixed
---
Launcher memory test (A3-T02): the heap budget now applies to the smallest of three 100-round windows instead of a single window, since V8 grows the heap in steps (27–250 KB per window on an idle app); listener and DOM-node checks run after every window. A real leak still fails.
