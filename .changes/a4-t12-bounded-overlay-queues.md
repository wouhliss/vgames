---
audience: internal
component: launcher
type: changed
---
Overlay broker events go through a bounded queue (16, extra events dropped; a closed channel counts as the broker stopping); tests prove the drop behaviour for broker events and in-game queued actions (A4-T12).
