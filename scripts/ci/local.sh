#!/usr/bin/env bash
# Local CI: the jobs of .github/workflows/ci.yml, run on your machine. Owner: Agent 5.
#
# GitHub Actions is manual-only since the Actions minutes ran out (2026-09-25), so this script is the
# merge gate: every PR must pass it on its final commit before it is merged (AGENTS.md §7). It checks
# what is COMMITTED (commit first), writes target/ci-local/summary.md to paste into the PR, and exits
# non-zero if any job failed. A job never skips itself: a missing tool or service fails it with the
# command that installs it. Logs: target/ci-local/<job>.log.
#
# usage: scripts/ci/local.sh [--base REV] [--list] [JOB ...]      (default: every job)
#
# Needs:
#   rust, sqlx     Postgres 18: DATABASE_URL (a role that may create databases), otherwise
#                  `docker compose up -d postgres` is started for you. sqlx: sqlx-cli 0.9.0.
#   wasm           wasm-pack; the wasm32 target comes with rust-toolchain.toml. wasm-pack downloads
#                  wasm-opt from GitHub unless one is on PATH (offline: install binaryen).
#   typescript,    Node 22 and pnpm (corepack enable). desktop-e2e, admin-e2e: Playwright Chromium
#   desktop-e2e,   (pnpm --filter @vgames/desktop exec playwright install chromium), or an existing
#   admin-e2e
#                  Chromium via PLAYWRIGHT_CHROMIUM_EXECUTABLE=/path/to/chrome.
#   desktop        WebKitGTK 4.1 development libraries (Linux; see AGENTS.md §4).
#   supply-chain   cargo-deny, cargo-audit, network access.
#   secrets,       gitleaks 8.30.1 and actionlint 1.7.12: downloaded and checksum-verified into
#   workflows      ~/.cache/vgames-ci on Linux x64; elsewhere use the same versions on PATH.
# Not covered: Windows and macOS builds (desktop-matrix.yml). Run `cargo test -p vgames-desktop
# -p vgames-transfer` on those systems, or start that workflow by hand when minutes are available.
set -uo pipefail

JOBS=(rust sqlx changelog wasm typescript desktop-e2e admin-e2e desktop supply-chain secrets workflows)
declare -A TITLE=(
  [rust]="Rust (fmt, clippy, tests)"
  [sqlx]="SQLx offline data and migrations"
  [changelog]="Changelog fragments, code owners and security gates"
  [wasm]="WASM (vgames-core, pack-wasm)"
  [typescript]="TypeScript (Biome, typecheck, Vitest, OpenAPI lint)"
  [desktop-e2e]="Launcher UI end-to-end (mock mode)"
  [admin-e2e]="Admin UI end-to-end (mock mode)"
  [desktop]="Desktop build check (Linux, WebKitGTK)"
  [supply-chain]="Supply chain (cargo-deny, cargo-audit, pnpm audit)"
  [secrets]="Secret scan (gitleaks)"
  [workflows]="Workflows (actionlint)"
)

GITLEAKS_VERSION=8.30.1
GITLEAKS_SHA256=551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb
ACTIONLINT_VERSION=1.7.12
ACTIONLINT_SHA256=8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8
SQLX_CLI_VERSION=0.9.0

root=$(git rev-parse --show-toplevel) || exit 2
cd "$root" || exit 2
out="$root/target/ci-local"
cache="${XDG_CACHE_HOME:-$HOME/.cache}/vgames-ci"
base=""
selected=()

while [ $# -gt 0 ]; do
  case "$1" in
    --base) base=$2; shift 2 ;;
    --list) for j in "${JOBS[@]}"; do printf '%-13s %s\n' "$j" "${TITLE[$j]}"; done; exit 0 ;;
    -h | --help) sed -n '2,/^set -uo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
    -*) echo "unknown option $1" >&2; exit 2 ;;
    *)
      [ -n "${TITLE[$1]:-}" ] || { echo "unknown job $1 (--list)" >&2; exit 2; }
      selected+=("$1"); shift ;;
  esac
done
[ ${#selected[@]} -gt 0 ] || selected=("${JOBS[@]}")

if [ -z "$base" ]; then
  git fetch --quiet origin main 2>/dev/null || echo "note: could not fetch origin/main; using the local copy"
  base=$(git merge-base HEAD origin/main 2>/dev/null) || { echo "no origin/main: pass --base REV" >&2; exit 2; }
fi

export CARGO_TERM_COLOR=never CARGO_INCREMENTAL=0 SQLX_OFFLINE=true CI=true

# ---- helpers (run inside a job's subshell, which has `set -e`) --------------------------------

need() { # command, install hint
  command -v "$1" >/dev/null 2>&1 || { echo "MISSING TOOL: $1 — install: $2"; return 1; }
}

pnpm_install() {
  need pnpm "corepack enable (Node 22)"
  pnpm install --frozen-lockfile
}

ensure_postgres() {
  if [ -z "${DATABASE_URL:-}" ]; then
    need docker "install Docker, or set DATABASE_URL to a Postgres 18 URL whose role may create databases"
    docker compose up -d --wait postgres
    export DATABASE_URL="postgres://vgames:${POSTGRES_PASSWORD:-vgames-dev-only}@127.0.0.1:5432/vgames"
  fi
  echo "Postgres: ${DATABASE_URL%%:*}://…@${DATABASE_URL##*@}"
}

pinned_tool() { # name version sha256 url-template(with {v}) -> prints the binary path
  local name=$1 version=$2 sum=$3 url=${4//\{v\}/$2}
  local bin="$cache/$name-$version"
  if [ -x "$bin" ]; then echo "$bin"; return; fi
  if [ "$(uname -s)-$(uname -m)" != Linux-x86_64 ]; then
    command -v "$name" >/dev/null 2>&1 || { echo "MISSING TOOL: $name $version (brew install $name)" >&2; return 1; }
    "$name" --version 2>&1 | grep -q "$version" || { echo "$name must be version $version" >&2; return 1; }
    command -v "$name"; return
  fi
  mkdir -p "$cache"
  curl -sSfL -o "$cache/$name.tgz" "$url"
  echo "$sum  $cache/$name.tgz" | sha256sum -c - >&2
  tar -xzf "$cache/$name.tgz" -C "$cache" "$name"
  mv "$cache/$name" "$bin"
  rm -f "$cache/$name.tgz"
  echo "$bin"
}

# ---- jobs (each mirrors the ci.yml job of the same name) --------------------------------------

job_rust() {
  ensure_postgres
  cargo xtask toolchain check
  cargo fmt --all -- --check
  cargo clippy --all-targets --locked -- -D warnings
  cargo test --locked
  # ci.yml "overlay renderer" (the Vulkan test is skipped without lavapipe here).
  cargo clippy -p vgames-overlay --all-targets --features renderer --locked -- -D warnings
  cargo test -p vgames-overlay --features renderer --locked
}

job_sqlx() {
  ensure_postgres
  need sqlx "cargo install sqlx-cli --version $SQLX_CLI_VERSION --locked --no-default-features --features rustls,postgres"
  local pg=${DATABASE_URL%/*} run=$$ tmp
  tmp=$(mktemp -d)
  # Expanded now: these locals no longer exist when the job's subshell exits. Cleanup never fails the job.
  # shellcheck disable=SC2064
  trap "sqlx database drop -y --database-url '$pg/ci_empty_$run' >/dev/null 2>&1 || true
        sqlx database drop -y --database-url '$pg/ci_upgrade_$run' >/dev/null 2>&1 || true
        rm -rf '$tmp'" EXIT
  echo "== migrations apply to an empty database"
  sqlx database create --database-url "$pg/ci_empty_$run"
  sqlx migrate run --source apps/api/migrations --database-url "$pg/ci_empty_$run"
  echo "== migrations upgrade from the base revision $base (merged migrations are never edited)"
  mkdir -p "$tmp/apps/api/migrations"
  if [ -n "$(git ls-tree "$base" apps/api/migrations/)" ]; then
    git archive "$base" apps/api/migrations | tar -x -C "$tmp"
  fi
  sqlx database create --database-url "$pg/ci_upgrade_$run"
  sqlx migrate run --source "$tmp/apps/api/migrations" --database-url "$pg/ci_upgrade_$run"
  sqlx migrate run --source apps/api/migrations --database-url "$pg/ci_upgrade_$run"
  echo "== committed .sqlx data matches the queries"
  SQLX_OFFLINE=false DATABASE_URL="$pg/ci_empty_$run" cargo sqlx prepare --workspace --check
}

job_changelog() {
  if [ "$(git rev-parse HEAD)" = "$(git rev-parse "$base")" ]; then
    cargo xtask changelog lint
  else
    cargo xtask changelog check --base "$base"
  fi
  cargo xtask codeowners check
  cargo xtask security check
}

job_wasm() {
  cargo clippy -p vgames-core --features wasm --target wasm32-unknown-unknown --locked -- -D warnings
  need wasm-pack "cargo install wasm-pack --locked"
  pnpm_install
  pnpm --filter @vgames/pack-wasm build
  pnpm --filter @vgames/pack-wasm smoke
}

job_typescript() {
  pnpm_install
  pnpm lint
  pnpm typecheck
  pnpm test
  pnpm openapi:lint
}

job_desktop-e2e() {
  pnpm_install
  pnpm --filter @vgames/desktop e2e
}

job_admin-e2e() {
  pnpm_install
  pnpm --filter @vgames/admin-web e2e
}

job_desktop() {
  need pkg-config "apt-get install pkg-config"
  pkg-config --exists webkit2gtk-4.1 || {
    echo "MISSING: WebKitGTK 4.1 — apt-get install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev libdbus-1-dev libudev-dev"
    return 1
  }
  pnpm_install
  pnpm --filter @vgames/desktop build
  cargo clippy -p vgames-desktop --all-targets --locked -- -D warnings
  cargo test -p vgames-desktop --lib --locked updater::
}

job_supply-chain() {
  need cargo-deny "cargo install cargo-deny --locked"
  need cargo-audit "cargo install cargo-audit --locked"
  cargo deny --workspace --locked check
  cargo audit
  need pnpm "corepack enable (Node 22)"
  pnpm audit --prod
}

job_secrets() {
  local gitleaks
  gitleaks=$(pinned_tool gitleaks "$GITLEAKS_VERSION" "$GITLEAKS_SHA256" \
    "https://github.com/gitleaks/gitleaks/releases/download/v{v}/gitleaks_{v}_linux_x64.tar.gz")
  "$gitleaks" git --config .gitleaks.toml --redact --no-banner --exit-code 1 .
}

job_workflows() {
  local actionlint
  actionlint=$(pinned_tool actionlint "$ACTIONLINT_VERSION" "$ACTIONLINT_SHA256" \
    "https://github.com/rhysd/actionlint/releases/download/v{v}/actionlint_{v}_linux_amd64.tar.gz")
  command -v shellcheck >/dev/null 2>&1 || echo "note: shellcheck not on PATH; actionlint skips run: scripts (CI has it)"
  "$actionlint" -no-color
}

# ---- run ----------------------------------------------------------------------------------------

mkdir -p "$out"
head=$(git rev-parse --short HEAD)
dirty=""
git diff --quiet HEAD -- 2>/dev/null || dirty=" (uncommitted changes present: they are not what was checked)"
summary="$out/summary.md"
{
  echo "### Local CI (\`scripts/ci/local.sh\`)"
  echo
  echo "Commit \`$head\` on \`$(git rev-parse --abbrev-ref HEAD)\`, base \`$(git rev-parse --short "$base")\`, $(date -u '+%Y-%m-%d %H:%M UTC'), $(uname -sm)$dirty."
  echo
  echo "| Job | Result | Time |"
  echo "|---|---|---|"
} >"$summary"

failed=0
for job in "${selected[@]}"; do
  log="$out/$job.log"
  printf '%-58s' "${TITLE[$job]}"
  start=$SECONDS
  ( set -euo pipefail; "job_$job" ) >"$log" 2>&1
  rc=$?
  took=$((SECONDS - start))
  time=$(printf '%dm%02ds' $((took / 60)) $((took % 60)))
  if [ "$rc" -eq 0 ]; then
    echo "PASS  $time"
    echo "| ${TITLE[$job]} | PASS | $time |" >>"$summary"
  else
    failed=$((failed + 1))
    echo "FAIL  $time  ($log)"
    echo "| ${TITLE[$job]} | **FAIL** | $time |" >>"$summary"
    tail -n 25 "$log" | sed 's/^/    /'
  fi
done

{
  echo
  if [ "$failed" -eq 0 ]; then
    echo "All ${#selected[@]} jobs passed."
  else
    echo "$failed of ${#selected[@]} jobs failed."
  fi
  [ ${#selected[@]} -eq ${#JOBS[@]} ] || echo "Only these jobs ran; the merge gate is all of them."
} >>"$summary"
echo
echo "Summary for the PR: $summary"
[ "$failed" -eq 0 ]
