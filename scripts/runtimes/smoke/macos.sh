#!/usr/bin/env bash
# macOS smoke test for new Wine catalog entries (A5-T12, runtimes.yml). Runs on an Apple silicon runner.
#
# usage: macos.sh <watch dir> <d3d11_smoke.exe>
#
# For every new wine-macos entry in <watch dir>/smoke.json: `wine --version`, a fresh prefix, then
# d3d11_smoke.exe (WineD3D). Hosted macOS runners are virtual machines that may expose no usable GPU:
# when the test cannot create a D3D11 device (exit 2) the entry is reported as NOT VERIFIED rather than
# passing. Archives are verified against their pinned SHA-256 by fetch.py first. Writes
# <watch dir>/smoke-macos.md; exits 1 when a test fails. (macOS has no `timeout`: the job's timeout-minutes bounds it.)
set -euo pipefail

watch_dir=$(cd "$1" && pwd)
exe=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
here=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
report="$watch_dir/smoke-macos.md"
: >"$report"
status=0

# x86_64 Wine needs Rosetta 2 on Apple silicon.
if [ "$(uname -m)" = arm64 ] && ! /usr/bin/pgrep -q oahd; then
  sudo softwareupdate --install-rosetta --agree-to-license >/dev/null
fi

while IFS=$'\t' read -r id version kind; do
  label="$id $version"
  if [ "$kind" != wine-macos ]; then
    echo "- NOT RUN $label: no macOS smoke test for '$kind' yet (review by hand)" >>"$report"
    continue
  fi
  if ! dir=$(python3 "$here/fetch.py" --entries "$watch_dir/entries.json" --id "$id" --dest "$work/runtimes"); then
    echo "- FAIL $label: the download did not verify against its pinned size and SHA-256" >>"$report"
    status=1
    continue
  fi
  wine=$(find "$dir" -type f -path '*/bin/wine' -print -quit)
  [ -n "$wine" ] || wine=$(find "$dir" -type f -path '*/bin/wine64' -print -quit)
  if [ -z "$wine" ]; then
    echo "- FAIL $label: no bin/wine in the archive" >>"$report"
    status=1
    continue
  fi
  prefix=$(mktemp -d "$work/prefix.XXXX")
  log=$(mktemp "$work/log.XXXX")
  set +e
  {
    "$wine" --version &&
      WINEPREFIX="$prefix" WINEDEBUG=-all "$wine" wineboot --init &&
      WINEPREFIX="$prefix" WINEDEBUG=-all "$wine" "$exe"
  } >"$log" 2>&1
  rc=$?
  set -e
  if [ "$rc" -eq 0 ] && grep -q "d3d11-smoke: ok" "$log"; then
    echo "- PASS $label: $(head -n 1 "$log"), prefix created, D3D11 rendered the expected pixel" >>"$report"
  elif [ "$rc" -eq 2 ] && grep -q "d3d11-smoke: no hardware" "$log"; then
    echo "- NOT VERIFIED $label: $(head -n 1 "$log") and a prefix work; no D3D11 device on this runner (no GPU)" >>"$report"
  else
    echo "- FAIL $label: exit $rc" >>"$report"
    { echo '  ```'; tail -n 40 "$log" | sed 's/^/  /'; echo '  ```'; } >>"$report"
    status=1
  fi
done < <(jq -r '.[] | select(.os == "macos") | [.id, .version, .smoke] | @tsv' "$watch_dir/smoke.json")

cat "$report"
exit "$status"
