---
audience: internal
component: server
type: changed
---
The API integration tests compile into one test binary instead of one per file, cutting build time and disk use (CI runners were running out of space).
