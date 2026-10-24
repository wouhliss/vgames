#!/usr/bin/env bash
# Keep test output attributable when Actions archives only whole-job logs.
set -euo pipefail
step=${1:?expected a public workflow step name}
shift
if [ "${VGAMES_CAPTURE_RECEIPTS:-0}" = 1 ]; then
  export VGAMES_RECEIPT_DIR="${RUNNER_TEMP:?}/milestone-receipts"
fi
[[ "$step" != *$'\n'* && "$step" != *$'\r'* ]] || exit 2
marker=$(printf '%s' "$step" | base64 | tr -d '\r\n')
printf 'VGAMES_STEP_START %s\n' "$marker"
finish() {
  status=$?
  printf 'VGAMES_STEP_END %s %s\n' "$marker" "$status"
  if [ -n "${VGAMES_RECEIPT_DIR:-}" ]; then
    if ! PYTHONUTF8=1 python3 scripts/demo/receipts.py finish "$step" "$status"; then
      [ "$status" -ne 0 ] || status=1
    fi
  fi
  exit "$status"
}
trap finish EXIT
run_command() {
  if [ "$#" -gt 0 ]; then
    "$@"
  else
    bash -euo pipefail
  fi
}
if [ -n "${VGAMES_RECEIPT_DIR:-}" ]; then
  run_command "$@" 2>&1 | PYTHONUTF8=1 python3 scripts/demo/receipts.py capture "$step"
else
  run_command "$@"
fi
