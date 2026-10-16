---
audience: internal
component: launcher
type: security
---
Join secrets follow `^[A-Za-z0-9_\[][A-Za-z0-9._:\-\[\]]{0,255}$` everywhere (proto grammar, launch substitution, invite dialog and mock); a secret stored under the older grammar is dropped when sending `invite.join`, on receipt and at launch, so it never becomes an option-shaped argument (GAME-14 threat review S3).
