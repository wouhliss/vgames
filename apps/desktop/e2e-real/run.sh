#!/usr/bin/env bash
# Runs the real-application suite (INS-09) end to end on Linux:
#
#   apps/desktop/e2e-real/run.sh <parts>     # 1 (M1, PRs) or 1,2 (M1 and M2, nightly)
#
# Needs: target/release/vgames-desktop (cargo build --release -p vgames-desktop --features
# tauri/custom-protocol, which embeds the UI as `tauri build` does; after the
# launcher UI is built), target/debug/vgames-api and target/debug/vgames (cargo build -p
# vgames-api -p vgames-cli), a PostgreSQL database in E2E_DATABASE_URL (it is migrated and
# written to; use a throwaway one), tauri-driver and WebKitWebDriver, stunnel, Xvfb,
# dbus-run-session, jq, and sudo for trusting the throwaway CA (fixtures/tls.sh).
set -euo pipefail
parts=${1:-1}
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
target=${CARGO_TARGET_DIR:-$repo/target}
if [[ -z "${DISPLAY:-}" ]]; then
  exec xvfb-run -a -s "-screen 0 1920x1080x24" "$0" "$@"
fi
: "${E2E_DATABASE_URL:?a throwaway PostgreSQL database URL}"
api_port=${E2E_API_PORT:-18080}
https_port=${E2E_HTTPS_PORT:-8443}
driver_port=${E2E_DRIVER_PORT:-4444}
work=${E2E_WORK_DIR:-$(mktemp -d)}
mkdir -p "$work"
instance_pid=""

cleanup() {
  [[ -n "$instance_pid" ]] && kill "$instance_pid" 2>/dev/null || true
  pkill -f "WebKitWebDriver --port=$((driver_port + 1))" 2>/dev/null || true
  "$here/fixtures/api.sh" down "$work/api" || true
  "$here/fixtures/tls.sh" down "$work/tls" || true
}
trap cleanup EXIT

"$here/fixtures/api.sh" up "$work/api" "$E2E_DATABASE_URL" "$api_port" "$https_port"
"$here/fixtures/tls.sh" up "$work/tls" "$https_port" "$api_port"

mkdir -p "$work/library"
"$here/fixtures/instance.sh" "$work/a" "$driver_port" > "$work/instance-a.log" 2>&1 &
instance_pid=$!
for _ in $(seq 1 100); do
  curl -sf -o /dev/null "http://127.0.0.1:$driver_port/status" && break
  sleep 0.2
done

status=0
E2E_APP="$target/release/vgames-desktop" \
E2E_DRIVER_PORT="$driver_port" \
E2E_SERVER="https://localhost:$https_port" \
E2E_INSTANCE_HOME="$work/a/home" \
E2E_LIBRARY="$work/library" \
E2E_WORK="$work/api" \
E2E_PARTS="$parts" \
VGAMES_BIN="$target/debug/vgames" \
NODE_EXTRA_CA_CERTS="$work/tls/ca.crt" \
  node --experimental-strip-types --test --test-concurrency=1 "$here/specs/launcher.test.ts" || status=$?
if [[ $status -ne 0 ]]; then
  echo "--- API log"; tail -n 50 "$work/api/api.log" || true
  echo "--- instance log"; tail -n 50 "$work/instance-a.log" || true
fi
exit $status
