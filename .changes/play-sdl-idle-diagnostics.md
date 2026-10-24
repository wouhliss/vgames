---
audience: internal
component: launcher
type: changed
---
The SDL idle-wait test now reports which SDL events and `/dev` changes woke it, and a failure in one SDL test no longer cascades into the other through a poisoned lock (PLAY-04).
