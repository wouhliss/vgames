---
audience: internal
component: server
type: added
---
The API serves the built admin web UI at /admin/ (VGAMES_ADMIN_DIST): client-side routes fall back to index.html (never cached), hashed assets are cached as immutable, every response carries the admin content security policy, and request paths cannot leave the build directory. A missing or unbuilt directory stops the server at startup.
