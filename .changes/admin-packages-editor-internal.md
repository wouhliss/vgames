---
audience: internal
component: admin
type: added
---
A3-T14: admin packages list (URL filters, cursor paging), create (Idempotency-Key per form), editor (merge patch with If-Match, 412 diff, field limits mirroring the API, server field errors, dirty guard, status and typed-slug delete); MSW handlers for `/v1/admin/packages` whose state survives reloads in mock mode; `test/render.tsx` shared helper.
