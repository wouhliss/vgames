#!/usr/bin/env bash
# Linux smoke test for new Proton and umu-launcher catalog entries (A5-T12, runtimes.yml).
#
# usage: linux.sh <watch dir> <d3d11_smoke.exe>
#
# For every new Linux entry in <watch dir>/smoke.json: runs d3d11_smoke.exe with umu-run and a Proton
# (the new one, or a pinned one when the new entry is umu-launcher itself) in Xvfb, on Mesa's lavapipe
# Vulkan driver, so DXVK is exercised end to end. Archives are downloaded and verified against their
# pinned SHA-256 by fetch.py before anything runs. Writes <watch dir>/smoke-linux.md; exits 1 when a
# test fails.
set -euo pipefail

watch_dir=$(cd "$1" && pwd)
exe=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
here=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
report="$watch_dir/smoke-linux.md"
: > "$report"
status=0

fetch() { # id -> extraction dir (exit 3: nothing to test with)
  python3 "$here/fetch.py" --entries "$watch_dir/entries.json" --id "$1" --dest "$work/runtimes"
}

find_one() { # dir name -> first regular file with that name
  find "$1" -type f -name "$2" -print -quit
}

run_smoke() { # label proton_dir umu_run
  local label=$1 proton=$2 umu_run=$3
  local prefix log rc
  prefix=$(mktemp -d "$work/prefix.XXXX")
  log=$(mktemp "$work/log.XXXX")
  set +e
  timeout 1200 xvfb-run -a -s "-screen 0 1024x768x24" \
    env GAMEID=umu-default PROTONPATH="$proton" WINEPREFIX="$prefix" "$umu_run" "$exe" >"$log" 2>&1
  rc=$?
  set -e
  if [ "$rc" -eq 0 ] && grep -q "d3d11-smoke: ok" "$log"; then
    echo "- PASS $label: D3D11 rendered the expected pixel (DXVK on lavapipe)" >>"$report"
  else
    echo "- FAIL $label: exit $rc" >>"$report"
    { echo '  ```'; tail -n 40 "$log" | sed 's/^/  /'; echo '  ```'; } >>"$report"
    status=1
  fi
}

fail() { # label message
  echo "- FAIL $1: $2" >>"$report"
  status=1
}

while IFS=$'\t' read -r id version kind; do
  label="$id $version"
  case "$kind" in
    proton)
      if ! proton_dir=$(fetch "$id"); then
        fail "$label" "the download did not verify against its pinned size and SHA-256"
        continue
      fi
      proton=$(dirname "$(find_one "$proton_dir" proton)")
      rc=0
      umu_dir=$(fetch umu-launcher) || rc=$?
      case "$rc" in
        0) run_smoke "$label" "$proton" "$(find_one "$umu_dir" umu-run)" ;;
        3) echo "- NOT RUN $label: no umu-launcher is pinned yet" >>"$report" ;;
        *) fail "$label" "umu-launcher did not verify against its pinned size and SHA-256" ;;
      esac
      ;;
    umu)
      if ! umu_dir=$(fetch "$id"); then
        fail "$label" "the download did not verify against its pinned size and SHA-256"
        continue
      fi
      proton_dir=""
      for candidate in ge-proton umu-proton; do
        rc=0
        proton_dir=$(fetch "$candidate") || rc=$?
        if [ "$rc" -eq 0 ]; then break; fi
        proton_dir=""
        if [ "$rc" -ne 3 ]; then
          fail "$label" "$candidate did not verify against its pinned size and SHA-256"
          continue 2
        fi
      done
      if [ -n "$proton_dir" ]; then
        run_smoke "$label" "$(dirname "$(find_one "$proton_dir" proton)")" "$(find_one "$umu_dir" umu-run)"
      else
        echo "- NOT RUN $label: no Proton is pinned yet" >>"$report"
      fi
      ;;
    *)
      echo "- NOT RUN $label: no Linux smoke test for '$kind'" >>"$report"
      ;;
  esac
done < <(jq -r '.[] | select(.os == "linux") | [.id, .version, .smoke] | @tsv' "$watch_dir/smoke.json")

cat "$report"
exit "$status"
