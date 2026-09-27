---
audience: internal
component: server
type: changed
---
Contract: a refused or failed Discord sign-in (`access_denied`, `registration_closed`, `not_allowlisted`, `user_disabled`, `sign_in_failed`) now consumes the flow and redirects to the client that started it with `error=<code>` (`vgames://auth/callback?error=…&client_state=…` with a page stating the reason, or `/admin/login?error=…`) instead of answering a 403 problem the launcher never saw. New `GET /v1/genres` (genres of the listed catalog with counts, optional `platform`, `Cache-Control: private, max-age=60`). `DELETE /v1/admin/packages/{id}` documents its `428`.
