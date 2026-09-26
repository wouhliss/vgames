---
audience: internal
component: server
type: changed
---
API connections now plan every statement with its parameters (`plan_cache_mode = force_custom_plan`), so the optional filters of the catalog, audit and job lists use their indexes. Generic plans sorted the whole catalog or walked the whole audit log: 35–160 ms on 100k packages and 1M audit rows, now under 1 ms (25 ms at worst). New `(key, id DESC)` indexes on the audit log's actor, action and target and on the job state. `apps/api/bench/` has the seed, the `EXPLAIN` review and an `oha` load test (p99 < 100 ms at 200 rps).
