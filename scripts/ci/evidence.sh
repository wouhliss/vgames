#!/usr/bin/env bash
# Keep test output attributable when Actions archives only whole-job logs.
set -euo pipefail
step=${1:?expected a public workflow step name}
shift
[[ "$step" != *$'\n'* && "$step" != *$'\r'* ]] || exit 2
marker=$(printf '%s' "$step" | base64 | tr -d '\r\n')
printf 'VGAMES_STEP_START %s\n' "$marker"
trap 'status=$?; printf "VGAMES_STEP_END %s %s\n" "$marker" "$status"; exit "$status"' EXIT
if [ "$#" -gt 0 ]; then
  "$@"
else
  bash -euo pipefail
fi
