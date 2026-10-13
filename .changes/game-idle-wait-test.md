---
audience: internal
component: launcher
type: fixed
---
The Linux controller idle-wait test no longer fails when the kernel delivers an event of the previous step late (deferred `IN_IGNORED`): the wait is restarted and must block within five tries.
