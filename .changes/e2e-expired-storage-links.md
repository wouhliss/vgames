---
audience: internal
component: server
type: added
---
Nightly E2E (A5-T11): a second API on the same database signs 60-second storage links; the key pipeline checks that a manifest link and a pack link work, that the fs storage backend refuses both once expired (403), and that freshly signed ones still work. With expiry ignored in the backend, the pipeline fails.
