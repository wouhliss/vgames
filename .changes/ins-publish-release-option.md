---
audience: internal
component: launcher
type: added
---
`vgames_transfer::upload::publish::PublishOptions::release` (default true): when false, publishing stops at a verified `ready` version (`PublishPhase::Ready`) instead of releasing it, as the CLI owner asked. `UploadProgress::packs` reports confirmed bytes per pack for the launcher's publish screen (INS-06).
