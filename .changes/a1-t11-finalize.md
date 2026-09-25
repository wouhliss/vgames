---
audience: internal
component: server
type: added
---
Version finalize and re-sign: the server verifies the uploaded manifest and its signature with vgames-core, checks every pack, records the version as verifying and queues pack verification; each failure has its own error code.
