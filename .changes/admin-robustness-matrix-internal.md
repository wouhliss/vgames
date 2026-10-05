---
audience: internal
component: admin
type: added
---
A3-T18: page × state matrix (every admin page's reads failing with 401, 403, 404, 429, 500, a proxy's HTML page, offline, non-JSON and off-contract bodies; slow loads; empty states) and windowed table rows above 200 entries (`useTableWindow`) for the packages, users, jobs and audit lists, tested at 10k rows. Test query clients keep their retries but skip the backoff.
