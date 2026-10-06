#!/usr/bin/env bash
# Job-only private keys never enter the artifact directory or logs.
set -euo pipefail
set +x
key_dir="$RUNNER_TEMP/updater-dry-key"
mkdir -m 700 "$key_dir"
trap 'rm -rf "$key_dir"' EXIT
export VERSION
VERSION=$(node -p 'JSON.parse(require("fs").readFileSync("apps/desktop/package.json")).version')
python3 - "$key_dir/password" <<'PY'
import base64, os, sys
from pathlib import Path
Path(sys.argv[1]).write_text(base64.b64encode(os.urandom(32)).decode())
os.chmod(sys.argv[1], 0o600)
PY
export TAURI_SIGNING_PRIVATE_KEY="$key_dir/updater.key"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD
TAURI_SIGNING_PRIVATE_KEY_PASSWORD=$(cat "$key_dir/password")
CI=true pnpm --filter @vgames/desktop tauri signer generate -w "$TAURI_SIGNING_PRIVATE_KEY" \
  -p "$TAURI_SIGNING_PRIVATE_KEY_PASSWORD" > /dev/null 2>&1
pubkey=$(cat "$key_dir/updater.key.pub")
export BUILD_CONFIG="$key_dir/build.json"
python3 - "$pubkey" "$BUILD_CONFIG" <<'PY'
import json, sys
from pathlib import Path
Path(sys.argv[2]).write_text(json.dumps({"plugins": {"updater": {"pubkey": sys.argv[1]}}}))
PY
bash scripts/release/desktop-build.sh
bash scripts/release/desktop-collect.sh
out="out/$TARGET"
printf '%s\n' "$pubkey" > "$out/updater.pub"
cargo xtask changelog export > "$key_dir/fragments.json"
node --experimental-strip-types scripts/release-notes/src/main.ts \
  --fragments "$key_dir/fragments.json" --out "$key_dir/notes.json"
cargo xtask changelog release "$VERSION" --notes "$key_dir/notes.json" \
  --date "$(date -u +%F)" --out-dir "$key_dir/changelog"
cp "$key_dir/changelog/changelog-user.json" "$out/"
case "$TARGET" in
  *windows*) artifacts=(--artifact "windows-x86_64=$(printf '%s' "$out"/*-setup.exe)") ;;
  *linux*) artifacts=(--artifact "linux-x86_64=$(printf '%s' "$out"/*.AppImage)"
                     --artifact "linux-x86_64-deb=$(printf '%s' "$out"/*.deb)"
                     --artifact "linux-x86_64-rpm=$(printf '%s' "$out"/*.rpm)") ;;
  *darwin*) artifacts=(--artifact "darwin-aarch64=$(printf '%s' "$out"/*.app.tar.gz)") ;;
  *) exit 1 ;;
esac
cargo xtask updater manifest --version "$VERSION" --pubkey "$pubkey" \
  --notes "$key_dir/changelog/latest-notes.txt" --pub-date "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --url-base "https://github.com/wouhliss/vgames/releases/download/desktop-v$VERSION" \
  "${artifacts[@]}" --out "$out/latest.json"
cargo xtask updater verify --latest "$out/latest.json" --dir "$out" --pubkey "$pubkey"
if [[ "$TARGET" == *linux* ]]; then
  sudo apt-get update
  sudo apt-get install -y --no-install-recommends xvfb "$PWD"/"$out"/*.deb
  if rg -q -- '--smoke-test' apps/desktop/src-tauri/src; then
    xvfb-run -a /usr/bin/vgames-desktop --smoke-test
  else
    test -x /usr/bin/vgames-desktop
    printf 'pending (PLAY-09): installed files verified; smoke-test is not on main.\n' >> "$GITHUB_STEP_SUMMARY"
  fi
  python3 - "$BUILD_CONFIG" <<'PY'
import json, sys
from pathlib import Path
path = Path(sys.argv[1]); conf = json.loads(path.read_text()); conf['version'] = '0.0.1'
path.write_text(json.dumps(conf))
PY
  export BUNDLES=appimage
  bash scripts/release/desktop-build.sh
  mkdir -p "$out/older"
  cp target/"$TARGET"/release/bundle/appimage/*0.0.1*.AppImage "$out/older/"
  printf 'pending (INS-09): real application updater harness is not on main; older signed-key build retained.\n' >> "$GITHUB_STEP_SUMMARY"
fi
