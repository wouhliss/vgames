#!/usr/bin/env bash
# Pins the latest upstream umu-launcher (zipapp) and UMU-Proton releases as runtime-catalog
# entries for compat::plan::proton_e2e (GAME-05): downloads each archive once, records its
# GitHub release URL, SHA-256, size and format, and prints a JSON array.
#
#   scripts/ci/proton-runtimes.sh [--tamper] > runtimes.json
#
# --tamper flips one hex digit of UMU-Proton's SHA-256, so the launcher must refuse the
# archive before extracting it. Needs curl, jq, sha256sum; GH_TOKEN avoids API rate limits.
set -euo pipefail

tamper=false
[[ "${1:-}" == "--tamper" ]] && tamper=true
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

api() {
  local auth=()
  [[ -n "${GH_TOKEN:-}" ]] && auth=(-H "Authorization: Bearer ${GH_TOKEN}")
  curl -fsSL --retry 3 "${auth[@]}" -H "Accept: application/vnd.github+json" "https://api.github.com/$1"
}

# entry <runtime id> <owner/repo> <asset regex> <license>
entry() {
  local id=$1 repo=$2 pattern=$3 license=$4 release tag asset url name format file
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
  sha=$(sha256sum "$file" | cut -d' ' -f1)
  size=$(stat -c %s "$file")
  if [[ "$tamper" == true && "$id" == umu-proton ]]; then
    local last=${sha: -1}
    sha="${sha:0:63}$([[ "$last" == 0 ]] && echo 1 || echo 0)"
  fi
  echo "pinned ${id} ${tag}: ${name} (${size} bytes, ${format})" >&2
  jq -n --arg id "$id" --arg version "$tag" --arg url "$url" --arg sha "$sha" \
    --argjson size "$size" --arg format "$format" --arg license "$license" \
    '{id: $id, version: $version, os: "linux", arch: "x86_64", url: $url, sha256: $sha,
      size: $size, archive: $format, license: $license, min_launcher_version: "0.1.0"}'
}

{
  entry umu-launcher Open-Wine-Components/umu-launcher 'zipapp.*\.(tar|zip|tar\.gz)$' GPL-3.0-only
  entry umu-proton Open-Wine-Components/umu-proton '^UMU-Proton-.*\.tar\.gz$' BSD-3-Clause
} | jq -s .
