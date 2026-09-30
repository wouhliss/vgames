---
audience: internal
component: server
type: changed
---
01-security §2 now lists HMAC-SHA-256, which the fs storage backend and page cursors already use (keys derived with BLAKE3 `derive_key`). Documentation only (A5-T13 review finding F6).
