---
audience: internal
component: server
type: changed
---
GAME-14: 03-api §6 now lists the server → client `typing` event (`{conversation_id, user_id}`, to the other current members, none across a block, throttled to one per user and conversation every 3 s on each API instance) and the optional `package_title` of `presence.changed`, both already sent by the API and handled by the launcher.
