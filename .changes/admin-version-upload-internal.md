---
audience: internal
component: admin
type: added
---
A3-T16 (versions and browser upload): Versions tab; upload wizard with a pack worker (plan, chunk hashing, pack streaming) and a key worker that only unlocks the publisher key and signs digests; GCS resumable sessions with 16 MiB pieces, 4 packs in parallel, IndexedDB resume state, Web Locks single-tab lock, automatic resume after network loss, new start URLs when expired, 422 finalize codes explained. pack-wasm is bundled through an optional glob when built before `vite build` (release builds must build it first); mock mode and tests use TypeScript stand-ins selected by the `__VGAMES_MOCK_UPLOADS__` build constant. Adds `@tanstack/react-virtual` to admin-web.
