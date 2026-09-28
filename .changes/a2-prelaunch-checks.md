---
audience: internal
component: launcher
type: added
---
A2-T09: pre-launch checks (02 §11). A launch requires an installed state and a stored manifest that verifies against the cached trust bundle; a revoked key asks for re-verification. The executable's BLAKE3 must match the manifest. Passed checks are cached by file stamps and trust version, so repeat launches skip hashing. The transfer testkit can now build packages with launch targets and revoked trust states.
