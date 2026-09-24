---
audience: internal
component: server
type: added
---
Trust bundle endpoints: the owner uploads root-signed bundles (verified with vgames-core, including root rotation), publisher keys are rebuilt from them, and the server fingerprint is now served on /.well-known/vgames.json.
