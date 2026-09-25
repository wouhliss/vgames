---
audience: internal
component: server
type: security
---
Every API route is rate limited: public routes per IP even when a request carries credentials, failed authentication per IP, signed storage URLs and static files (admin UI, API docs) with their own generous limits. Framework rejections (malformed path parameters, query strings, non-multipart uploads, plain HTTP on the realtime socket) now answer problem+json. Property tests cover slugs, cursors and the JSON, query and multipart extractors.
