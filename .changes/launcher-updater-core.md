---
audience: internal
component: launcher
type: added
---
Launcher self-update core: minisign-verified updates bound to their signed version (requireSignedVersion), HTTPS-only with no insecure redirects, strictly newer versions only, a 512 MiB artifact cap, checks every six hours when idle, a plain-text What's new from the player changelog, and typed commands for the update banner. Tested against a local update server with throwaway keys.
