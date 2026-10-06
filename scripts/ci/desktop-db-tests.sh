#!/usr/bin/env bash
# One home for ignored database scenarios; existing normal tests run in the whole suite.
# INT-03: never include social_soak here.
set -euo pipefail
: "${VGAMES_TEST_DATABASE_URL:?set a PostgreSQL 18 maintenance URL}"
args=(--test social_chat --test social_chaos)
for scenario in install_e2e publish_e2e saves_e2e; do
  if [[ -f "apps/desktop/src-tauri/tests/$scenario.rs" ]]; then
    args+=(--test "$scenario")
  fi
done
cargo test -p vgames-desktop --locked "${args[@]}" -- --ignored
