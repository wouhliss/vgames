#!/usr/bin/env bash
# INS-07 (Q13): runs one vgames-transfer download test many times in a row from one build, so an
# intermittent failure shows up. Usage: transfer-loop.sh <iterations> [cargo profile flags…]
#   scripts/ci/transfer-loop.sh 200            # debug
#   scripts/ci/transfer-loop.sh 200 --release
set -euo pipefail
iterations="${1:?iterations}"
shift
test_name="${LOOP_TEST:-protocol_violations_are_retried_once_on_a_fresh_connection}"
bin=$(cargo test -p vgames-transfer --locked --test download --no-run --message-format=json "$@" \
  | jq -r 'select(.reason == "compiler-artifact" and .executable != null and .target.name == "download") | .executable' \
  | tail -n 1)
[[ -x "$bin" ]] || { echo "no test binary" >&2; exit 1; }
for i in $(seq 1 "$iterations"); do
  if ! "$bin" --exact "$test_name" --quiet >"loop-$i.log" 2>&1; then
    echo "::error::$test_name failed on iteration $i of $iterations ($*)"
    cat "loop-$i.log"
    exit 1
  fi
  rm -f "loop-$i.log"
done
echo "$test_name passed $iterations consecutive iterations ($*)"
