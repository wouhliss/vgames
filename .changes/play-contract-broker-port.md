---
audience: internal
component: launcher
type: security
---
Contract (05-social §6.1, 05-social-notes): the overlay broker keeps its loopback listener bound for the whole game and closes extra connections while a renderer is served, instead of releasing the port, which another local process could take to receive a reconnecting renderer's token.
