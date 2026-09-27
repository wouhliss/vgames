---
audience: internal
component: admin
type: added
---
`vgames verify --package … --platform …` checks a published release the way launchers do before installing it (trust bundle under the pinned root, manifest size and BLAKE3, `verify_manifest` in install mode) and exits 1 when launchers would refuse it. The nightly key-pipeline e2e now covers the whole A5-T06 acceptance against the real API: publish with the first key, verified; bundle v2 revokes it and launchers refuse the release; re-sign with the new key; launchers accept it again.
