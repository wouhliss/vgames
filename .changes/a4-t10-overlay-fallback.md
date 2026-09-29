---
audience: internal
component: launcher
type: added
---
A4-T10: overlay window UI (`src/overlay/`: toasts, invites, quick reply, friends online, Open vgames) and the fallback
when no in-game renderer connects within 20 s (or on macOS): an always-on-top window at the screen edge that lets
clicks through unless the panel is open (Windows, X11, macOS), or OS notifications for toasts (Wayland).
