---
audience: internal
component: admin
type: added
---
`vgames publish <folder>` packs a folder, uploads it, signs its manifest with the publisher key and finalizes the version (Agent 2's publishing library). Before uploading anything it checks the folder (no symlinks or special files, launch targets present) and that the key is in the server's verified trust bundle, not revoked, valid and held by the caller. It resumes the same version after an interruption, stops at `ready` unless `--publish`, supports `--restart`, `--execution` (TOML or JSON launch settings) and `--json`, and is tested end to end against a mock API and storage.
