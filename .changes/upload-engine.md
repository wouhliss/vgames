---
audience: internal
component: admin
type: added
---
vgames-transfer upload engine and publish flow (02 §6): packs stream from the source folder into resumable storage sessions (16 MiB pieces, 4 to 16 packs in parallel), resume from a 0600 resume file after interruptions, abort when a source file changes, sign the manifest locally, finalize and wait for server verification. Library API for the CLI and the launcher admin mode.
