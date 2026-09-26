# API benchmarks (A1-T16)

Query plans and load numbers on a seeded database: 100k packages, 1M audit rows, 1M message
envelopes, 20k users and 200k jobs. The files are:

- `seed.sql`: the synthetic data (fake digests and signatures).
- `explain.sql`: `EXPLAIN (ANALYZE)` of the hot queries, written exactly as the handlers send them.
- `load.sh`: the load test, a p99 < 100 ms budget at 200 rps.

## Setup

```sh
docker compose up -d
createdb -h 127.0.0.1 -U vgames vgames_bench
sqlx migrate run --source apps/api/migrations \
  --database-url postgres://vgames:vgames-dev-only@127.0.0.1:5432/vgames_bench
psql -h 127.0.0.1 -U vgames -d vgames_bench -v ON_ERROR_STOP=1 -f apps/api/bench/seed.sql   # ~3 min
```

## Query plans

```sh
psql -h 127.0.0.1 -U vgames -d vgames_bench -f apps/api/bench/explain.sql
psql -h 127.0.0.1 -U vgames -d vgames_bench -v generic=1 -f apps/api/bench/explain.sql
```

The first command plans the queries the way the API does. `db::connect` sets
`plan_cache_mode = force_custom_plan`, so each call is planned with its own parameters.
The second shows what the generic plans would do.

## Load test

Start a release build against the seeded database. Use `.env.example` with throwaway secrets and
the `fs` storage backend, so download URLs are signed locally. Release builds refuse the fake
Discord, so give Discord placeholder values; the load test never signs in:

```sh
set -a; . ./.env.example; set +a
DATABASE_URL=postgres://vgames:vgames-dev-only@127.0.0.1:5432/vgames_bench \
  VGAMES_SERVER_SECRET=$(openssl rand -base64 32) VGAMES_FS_URL_SIGNING_KEY=$(openssl rand -base64 32) \
  DISCORD_CLIENT_ID=100000000000000000 DISCORD_CLIENT_SECRET=unused \
  DISCORD_REDIRECT_URI=http://127.0.0.1:8080/v1/auth/discord/callback \
  VGAMES_STORAGE_BACKEND=fs VGAMES_LOG=warn \
  cargo run --release -p vgames-api
BENCH_DATABASE_URL=postgres://vgames:vgames-dev-only@127.0.0.1:5432/vgames_bench \
  apps/api/bench/load.sh http://127.0.0.1:8080
```

The script needs `oha`, `psql` and `python3`. You can tune it with these environment variables:

| Variable | Default | Meaning |
|---|---|---|
| `RATE` | `200` | Requests per second, across all callers. |
| `DURATION` | `30s` | Measured length of each scenario. |
| `WARMUP` | `10s` | Unmeasured run before each scenario, to fill the caches. |
| `CALLERS` | `25` | Parallel bench sessions. |
| `BUDGET_MS` | `100` | The p99 limit. |

Each caller is a separate user, so no caller reaches the per-user rate limits. The script exits
non-zero if any p99 is over the budget or any request fails.

The results for each change are recorded in `docs/agents/status/agent-1.md`.
