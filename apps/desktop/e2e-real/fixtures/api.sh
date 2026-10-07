#!/usr/bin/env bash
# The real API for the real-application suite (INS-09): a debug build (the fake Discord
# login only exists there), Postgres, fs storage, behind tls.sh on https://localhost.
#
#   api.sh up <dir> <database-url> <api-port> <https-port>
#   api.sh down <dir>
#
# Expects target/debug/vgames-api (cargo build -p vgames-api). Writes <dir>/root.pub (the
# server's root public key) and <dir>/keys (the CLI's key files, test-only passphrases)
# through scripts/e2e/key-pipeline.sh init.
set -euo pipefail
cmd=${1:?up|down}
dir=${2:?work directory}
repo=$(cd "$(dirname "$0")/../../../.." && pwd)
target=${CARGO_TARGET_DIR:-$repo/target}

case "$cmd" in
up)
  db=${3:?database url}
  api_port=${4:?api port}
  https_port=${5:?https port}
  mkdir -p "$dir"
  VGAMES_BIN="$target/debug/vgames" "$repo/scripts/e2e/key-pipeline.sh" init "$dir/keys" > "$dir/root.pub"
  export DATABASE_URL="$db" DATABASE_MIGRATION_URL="$db"
  export VGAMES_BIND_ADDR="127.0.0.1:$api_port"
  export VGAMES_PUBLIC_URL="https://localhost:$https_port"
  export VGAMES_SERVER_NAME="vgames e2e"
  export VGAMES_SERVER_ID=01920000-0000-7000-8000-0000000e2e09
  export VGAMES_ROOT_PUBLIC_KEY
  VGAMES_ROOT_PUBLIC_KEY=$(cat "$dir/root.pub")
  export VGAMES_DEV_FAKE_DISCORD=true
  export VGAMES_BOOTSTRAP_OWNER_DISCORD_ID=100000000000000001
  export VGAMES_STORAGE_BACKEND=fs VGAMES_FS_STORAGE_ROOT="$dir/objects"
  export VGAMES_METADATA_STEAM_ENABLED=false
  export VGAMES_SERVER_SECRET VGAMES_FS_URL_SIGNING_KEY
  VGAMES_SERVER_SECRET=$(openssl rand -base64 32)
  VGAMES_FS_URL_SIGNING_KEY=$(openssl rand -base64 32)
  "$target/debug/vgames-api" --migrate
  nohup "$target/debug/vgames-api" > "$dir/api.log" 2>&1 &
  echo $! > "$dir/api.pid"
  for _ in $(seq 1 100); do
    curl -sf -o /dev/null "http://127.0.0.1:$api_port/v1/health" && exit 0
    sleep 0.2
  done
  echo "the API did not start" >&2
  cat "$dir/api.log" >&2
  exit 1
  ;;
down)
  [[ -f "$dir/api.pid" ]] && kill "$(cat "$dir/api.pid")" 2>/dev/null || true
  ;;
*)
  echo "usage: api.sh up <dir> <database-url> <api-port> <https-port> | down <dir>" >&2
  exit 2
  ;;
esac
