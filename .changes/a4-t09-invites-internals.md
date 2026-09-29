---
audience: internal
component: launcher
type: added
---
A4-T09: invite client (`invites_list`, `invite_send`, `invite_accept`, `invite_decline`, `invite_cancel`; events
`invite-received`, `invite-changed`, `invite-install-requested`). Join secrets stay on the sending install (sealed,
desktop migration `0004_social_invites`) and travel only in the Olm `invite.join`. Install progress from the bus is
reported every ≥ 5 s or 5 %. A `Games` port stands in for Agent 2's launch API. An invalid join secret in a received
`invite.join` now means a normal launch instead of an ignored message.
