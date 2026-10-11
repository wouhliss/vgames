#!/usr/bin/env bash
# Pins the latest upstream compatibility runtimes as runtime-catalog entries for the hosted
# compat tests (GAME-05, GAME-06): downloads each archive once, records its GitHub release URL,
# SHA-256, size and format, and prints a JSON array.
#
#   scripts/ci/compat-runtimes.sh proton > runtimes.json   # umu-launcher + UMU-Proton (Linux)
#   scripts/ci/compat-runtimes.sh wine > runtimes.json     # WineHQ macOS + DXMT + DXVK-macOS
#
# Needs curl, jq and sha256sum (shasum on macOS); GH_TOKEN avoids API rate limits.
set -euo pipefail

set_name=${1:?usage: compat-runtimes.sh proton|wine}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

api() {
  local auth=()
  [[ -n "${GH_TOKEN:-}" ]] && auth=(-H "Authorization: Bearer ${GH_TOKEN}")
  curl -fsSL --retry 3 "${auth[@]}" -H "Accept: application/vnd.github+json" "https://api.github.com/$1"
}

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# entry <runtime id> <owner/repo> <asset regex> <license> <os> [rosetta]
entry() {
  local id=$1 repo=$2 pattern=$3 license=$4 os=$5 rosetta=${6:-false} release tag asset url name format file
  release=$(api "repos/${repo}/releases/latest")
  tag=$(jq -r .tag_name <<<"$release")
  asset=$(jq -c --arg re "$pattern" '[.assets[] | select(.name | test($re))][0] // empty' <<<"$release")
  if [[ -z "$asset" ]]; then
    echo "no asset of ${repo} ${tag} matches ${pattern}; assets:" >&2
    jq -r '.assets[].name' <<<"$release" >&2
    exit 1
  fi
  url=$(jq -r .browser_download_url <<<"$asset")
  name=$(jq -r .name <<<"$asset")
  case "$name" in
    *.tar.gz) format=tar.gz ;;
    *.tar.xz) format=tar.xz ;;
    *.tar.zst) format=tar.zst ;;
    *.tar) format=tar ;;
    *.zip) format=zip ;;
    *) echo "unsupported archive ${name}" >&2; exit 1 ;;
  esac
  file="$work/$name"
  curl -fsSL --retry 3 -o "$file" "$url"
  local sha size
  sha=$(sha256 "$file")
  size=$(wc -c <"$file" | tr -d ' ')
  # Versions are tags without a leading "v" (catalog version text).
  tag=${tag#v}
  echo "pinned ${id} ${tag}: ${name} (${size} bytes, ${format})" >&2
  jq -n --arg id "$id" --arg version "$tag" --arg url "$url" --arg sha "$sha" \
    --argjson size "$size" --arg format "$format" --arg license "$license" --arg os "$os" \
    --argjson rosetta "$rosetta" \
    '{id: $id, version: $version, os: $os, arch: "x86_64", url: $url, sha256: $sha,
      size: $size, archive: $format, license: $license, min_launcher_version: "0.1.0"}
     + (if $rosetta then {rosetta_required: true} else {} end)'
}

case "$set_name" in
  proton)
    {
      entry umu-launcher Open-Wine-Components/umu-launcher 'zipapp.*\.(tar|zip|tar\.gz)$' GPL-3.0-only linux
      entry umu-proton Open-Wine-Components/umu-proton '^UMU-Proton-.*\.tar\.gz$' BSD-3-Clause linux
    } | jq -s .
    ;;
  wine)
    {
      entry wine-macos Gcenx/macOS_Wine_builds '^wine-(stable|devel)-.*osx64\.tar\.xz$' LGPL-2.1-or-later macos true
      entry dxmt 3Shain/dxmt '\.tar\.gz$' LGPL-2.1-or-later macos true
      entry dxvk-macos Gcenx/DXVK-macOS '\.tar\.gz$' Zlib macos true
    } | jq -s .
    ;;
  *) echo "unknown set ${set_name}" >&2; exit 1 ;;
esac
