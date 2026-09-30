#!/usr/bin/env bash
# D3DMetal intake (A5-T12, runtimes-d3dmetal.yml; 09-compatibility §3 and §7). Runs on an Apple silicon runner.
#
# usage: d3dmetal_intake.sh <Game Porting Toolkit .dmg> <version> <license path in the image> <repo> <out dir>
#                           [min launcher version, default 0.1.0]
#
# Apple's license lets vgames (non-commercial) redistribute D3DMetal unmodified and complete, with Apple's
# license text next to it. This script only packs what Apple signed:
#   1. mounts the image read-only and finds exactly one D3DMetal.framework in it;
#   2. requires a valid, strict, deep code signature made by Apple itself (`anchor apple`: Apple's own
#      certificate chain, not any Developer ID), so a modified or re-signed framework is refused;
#   3. packs the framework and the license text into d3dmetal-<version>-macos-aarch64.tar.xz;
#   4. unpacks that archive into a fresh directory and checks the signature again there, so the
#      archive is proven to carry the framework unmodified;
#   5. writes <out>/entries.json (the catalog entry, checked again by the launcher's parser when the
#      pull request is built) and <out>/summary.md.
# Nothing from the image is executed. Exits non-zero on any doubt.
set -euo pipefail

[ $# -ge 5 ] || { sed -n 4,5p "$0" | sed 's/^# //' >&2; exit 2; }
dmg=$1 version=$2 license_rel=$3 repo=$4 out=$5 min_launcher=${6:-0.1.0}

[[ $version =~ ^[A-Za-z0-9._+-]{1,64}$ ]] || { echo "invalid version: $version" >&2; exit 1; }
[[ $repo =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || { echo "invalid repository: $repo" >&2; exit 1; }
[[ $min_launcher =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "invalid launcher version: $min_launcher" >&2; exit 1; }
case $license_rel in /* | *..*) echo "the license path must be relative to the image, without '..'" >&2; exit 1 ;; esac
[ "$(uname -s)/$(uname -m)" = Darwin/arm64 ] || { echo "run this on an Apple silicon Mac" >&2; exit 1; }

mkdir -p "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d)
mnt="$work/image"
mkdir "$mnt"
cleanup() {
  hdiutil detach "$mnt" -force >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

hdiutil attach "$dmg" -readonly -nobrowse -noautoopen -mountpoint "$mnt" >/dev/null

# Exactly one framework: several would mean an image this script does not understand.
frameworks=()
while IFS= read -r -d '' f; do frameworks+=("$f"); done \
  < <(find "$mnt" -type d -name D3DMetal.framework -prune -print0)
[ "${#frameworks[@]}" -eq 1 ] || { echo "expected one D3DMetal.framework in the image, found ${#frameworks[@]}" >&2; exit 1; }
framework=${frameworks[0]}
echo "framework: ${framework#"$mnt"/}"

license="$mnt/$license_rel"
if [ ! -f "$license" ] || [ -L "$license" ]; then
  echo "no license file at $license_rel in the image" >&2
  exit 1
fi

verify() { # verify <framework>: valid, strict, deep, and signed by Apple itself
  codesign --verify --deep --strict --verbose=2 "$1"
  codesign --verify --deep --strict -R="anchor apple" "$1"
}
verify "$framework"

name="d3dmetal-$version-macos-aarch64.tar.xz"
stage="$work/stage"
mkdir "$stage"
ditto "$framework" "$stage/D3DMetal.framework" # keeps symlinks and modes
license_name="LICENSE-Apple-$(basename "$license")"
cp "$license" "$stage/$license_name"
# No extended attributes and no AppleDouble `._*` files: launchers unpack with their own extractor, and an
# extra file inside the bundle would break its sealed-resource signature. The signature does not depend on
# them; the check below proves it on the unpacked copy.
COPYFILE_DISABLE=1 tar --no-xattrs --no-mac-metadata -cJf "$out/$name" -C "$stage" D3DMetal.framework "$license_name"
if tar -tJf "$out/$name" | grep -qE '(^|/)\._'; then
  echo "the archive contains AppleDouble (._) entries" >&2
  exit 1
fi

check="$work/check"
mkdir "$check"
tar -xJf "$out/$name" -C "$check"
verify "$check/D3DMetal.framework"
diff -r "$framework" "$check/D3DMetal.framework" >/dev/null ||
  { echo "the unpacked framework differs from the one in the image" >&2; exit 1; }

sha256=$(shasum -a 256 "$out/$name" | cut -d' ' -f1)
size=$(stat -f %z "$out/$name")
authority=$(codesign -dvv "$framework" 2>&1 | sed -n 's/^Authority=//p' | paste -sd '>' -)

python3 - "$out/entries.json" "$version" "$repo" "$name" "$sha256" "$size" "$min_launcher" <<'PY'
import json, sys
path, version, repo, name, sha256, size, min_launcher = sys.argv[1:]
entry = {
    "id": "d3dmetal",
    "version": version,
    "os": "macos",
    "arch": "aarch64",
    "url": f"https://github.com/{repo}/releases/download/runtimes/{name}",
    "sha256": sha256,
    "size": int(size),
    "archive": "tar.xz",
    "license": "LicenseRef-Apple-D3DMetal",
    "min_launcher_version": min_launcher,
    "redistribution": "non-commercial",
}
with open(path, "w", encoding="utf-8") as f:
    json.dump([entry], f, indent=2)
PY

{
  echo "### D3DMetal $version"
  echo
  echo "- \`$name\`: $size bytes, SHA-256 \`$sha256\`"
  echo "- Signature: valid, strict, deep, \`anchor apple\` (${authority:-authority not shown}), checked in the image and again after unpacking the archive"
  echo "- Contents: \`D3DMetal.framework\` unmodified (\`diff -r\` against the image) and Apple's license text (\`$license_rel\`)"
  echo "- Catalog: macOS aarch64 only, \`redistribution = \"non-commercial\"\`"
} >"$out/summary.md"
echo "packed $name ($size bytes, sha256 $sha256)"
