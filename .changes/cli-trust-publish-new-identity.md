---
audience: internal
component: server
type: added
---
`vgames trust publish --new-identity` (with `--root`): publishes the first bundle under a replacement root after the old one was lost or stolen. The server must already advertise that root and the bundle must be newer than the server's current one; the old root's signature on the current bundle is no longer checked. Before, the CLI refused because the current bundle did not verify under the new root.
