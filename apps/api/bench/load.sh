#!/usr/bin/env bash
# Load test of the hot read paths (A1-T16): p99 < 100 ms at 200 rps on the catalog, the release
# descriptor and download URLs. See README.md for the setup.
#
#   BENCH_DATABASE_URL=postgres://…/vgames_bench apps/api/bench/load.sh [base_url]
#
# Needs oha (https://github.com/hatoo/oha), psql and python3. Each scenario runs CALLERS parallel
# oha processes, one bench session each, sharing RATE requests per second: first WARMUP (not
# measured, fills the caches), then DURATION, with latency correction (no coordinated omission).
# The per-request CSVs are merged, so the percentiles are exact over all requests. Exits non-zero
# when a p99 is over the budget or a request fails.
set -euo pipefail

BASE=${1:-http://127.0.0.1:8080}
RATE=${RATE:-200}
DURATION=${DURATION:-30s}
WARMUP=${WARMUP:-10s}
CALLERS=${CALLERS:-25}
BUDGET_MS=${BUDGET_MS:-100}
DB=${BENCH_DATABASE_URL:?set BENCH_DATABASE_URL to the seeded database}

for tool in oha psql python3; do
  command -v "$tool" >/dev/null || { echo "missing $tool" >&2; exit 2; }
done
if (( RATE % CALLERS != 0 )); then
  echo "RATE must be a multiple of CALLERS" >&2
  exit 2
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Bench user n has discord id 100000000000 + n and the access token below (seed.sql). Users 1-5
# are admins; callers start at 1000 and each scenario and phase uses its own users, so the
# per-user limits (600/min, 2000 download-URL calls/hour) are never reached.
token() { printf 'vga_%s' "$(printf 'bench%d' $((100000000000 + $1)) | sed -e :a -e 's/^.\{1,42\}$/&A/;ta')"; }

q() { psql "$DB" -XAtq -v ON_ERROR_STOP=1 -c "$1"; }

# Targets: random published packages and their current versions.
q "SELECT r.package_id, r.platform, r.version_id FROM package_releases r
   JOIN packages p ON p.id = r.package_id AND p.deleted_at IS NULL AND p.status = 'published'
   ORDER BY random() LIMIT 2000" > "$work/targets"
[[ -s $work/targets ]] || { echo "no published releases in $DB (run seed.sql)" >&2; exit 2; }

# Catalog mix: first pages by title and by recency, search, genre and platform filters.
python3 - "$BASE" > "$work/catalog.urls" <<'EOF'
import random, sys
base = sys.argv[1]
random.seed(7)
words = ['portal', 'hollow', 'night', 'star', 'iron', 'crystal', 'shadow', 'pixel', 'dungeon',
         'ocean', 'knight', 'racer', 'quest', 'garden', 'tactics', 'runner']
genres = ['action', 'puzzle', 'rpg', 'strategy', 'indie']
for _ in range(2000):
    r = random.random()
    if r < 0.35:
        path = '/v1/packages?platform=windows-x86_64'
    elif r < 0.5:
        path = '/v1/packages?sort=recent&platform=windows-x86_64'
    elif r < 0.75:
        a, b = random.sample(words, 2)
        path = f'/v1/packages?q={a}%20{b}' if random.random() < 0.5 else f'/v1/packages?q={a}'
    elif r < 0.9:
        path = f'/v1/packages?genre={random.choice(genres)}&platform=windows-x86_64'
    else:
        path = f'/v1/packages?platform=linux-x86_64&genre={random.choice(genres)}'
    print(base + path)
EOF
awk -v b="$BASE" -F'|' '{print b "/v1/packages/" $1 "/releases/" $2}' "$work/targets" > "$work/release.urls"
awk -v b="$BASE" -F'|' '{print b "/v1/versions/" $3 "/download-urls"}' "$work/targets" > "$work/download.urls"

per_caller=$((RATE / CALLERS))
status=0
scenario=0

# phase <dir> <duration> <first user> <urls> [oha args…]
phase() {
  local dir=$1 duration=$2 first=$3 urls=$4
  shift 4
  mkdir -p "$dir"
  local pids=()
  for i in $(seq 1 "$CALLERS"); do
    oha --no-tui --urls-from-file -q "$per_caller" -z "$duration" --latency-correction -c 4 \
      --output-format csv -H "authorization: Bearer $(token $((first + i)))" "$@" \
      "$urls" > "$dir/$i.csv" &
    pids+=($!)
  done
  for p in "${pids[@]}"; do wait "$p"; done
}

run() {
  local name=$1 urls=$2
  shift 2
  scenario=$((scenario + 1))
  local dir="$work/$name"
  phase "$dir.warmup" "$WARMUP" $((1000 * scenario + 500)) "$urls" "$@"
  phase "$dir" "$DURATION" $((1000 * scenario)) "$urls" "$@"
  python3 - "$name" "$BUDGET_MS" "$dir"/*.csv <<'EOF' || status=1
import csv, sys
name, budget, files = sys.argv[1], float(sys.argv[2]), sys.argv[3:]
lat, codes = [], {}
for f in files:
    with open(f) as fh:
        for row in csv.DictReader(fh):
            lat.append(float(row['request-duration']) * 1000)
            codes[row['status']] = codes.get(row['status'], 0) + 1
if not lat:
    print(f'{name}: no responses'); sys.exit(1)
lat.sort()
pct = lambda p: lat[min(len(lat) - 1, int(p / 100 * len(lat)))]
ok = sum(n for c, n in codes.items() if c.startswith('2'))
print(f'{name:<20} n={len(lat):<6} p50={pct(50):6.1f} ms  p90={pct(90):6.1f} ms  '
      f'p99={pct(99):6.1f} ms  max={lat[-1]:6.1f} ms  status={dict(sorted(codes.items()))}')
sys.exit(0 if pct(99) < budget and ok == len(lat) else 1)
EOF
}

echo "rate ${RATE} rps for ${DURATION} per scenario (after ${WARMUP} warm-up), ${CALLERS} callers, budget p99 < ${BUDGET_MS} ms"
run catalog "$work/catalog.urls"
run release-descriptor "$work/release.urls"
# Four packs per call: download URLs are limited per URL (2,000 per user per hour).
run download-urls "$work/download.urls" -m POST -T application/json -d '{"packs":[0,1,2,3]}'
exit $status
