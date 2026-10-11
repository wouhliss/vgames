#!/usr/bin/env bash
# The dry-run VM only: unchanged GitHub URLs, real HTTPS, real signed AppImage.
set -euo pipefail
set +x
umask 077
here=$(cd "$(dirname "$0")" && pwd)
artifacts=$(realpath "${1:?dry-run artifact directory}")
older=$(realpath "${2:?lower-version AppImage}")
work=$(mktemp -d)
marker="vgames-updater-e2e-$$"
ca="/usr/local/share/ca-certificates/$marker.crt"
cleanup() {
  if [[ -f "$work/server.pid" ]]; then sudo kill "$(sudo cat "$work/server.pid")" 2>/dev/null || true; fi
  sudo sed -i "/# $marker\$/d" /etc/hosts
  sudo rm -f "$ca"
  sudo update-ca-certificates >/dev/null 2>&1 || true
  if [[ -f "$work/requests.log" ]]; then
    sudo install -m 644 "$work/requests.log" "$artifacts/updater-requests.log"
  fi
  sudo rm -rf "$work"
}
trap cleanup EXIT
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 1 \
  -subj "/CN=vgames updater dry-run CA" -addext "basicConstraints=critical,CA:TRUE" \
  -addext "keyUsage=critical,keyCertSign" -keyout "$work/ca.key" -out "$work/ca.crt" 2>/dev/null
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj "/CN=github.com" \
  -keyout "$work/server.key" -out "$work/server.csr" 2>/dev/null
openssl x509 -req -in "$work/server.csr" -CA "$work/ca.crt" -CAkey "$work/ca.key" \
  -CAcreateserial -days 1 -out "$work/server.crt" \
  -extfile <(printf 'subjectAltName=DNS:github.com\nextendedKeyUsage=serverAuth\n') 2>/dev/null
rm -f "$work/ca.key" "$work/server.csr"
sudo cp "$work/ca.crt" "$ca"
sudo update-ca-certificates >/dev/null 2>&1
printf '127.0.0.1 github.com # %s\n' "$marker" | sudo tee -a /etc/hosts >/dev/null
sudo python3 "$here/updater-server.py" --artifacts "$artifacts" \
  --certificate "$work/server.crt" --key "$work/server.key" \
  --receipts "$work/requests.log" --pid "$work/server.pid" &
server_job=$!
ready=false
for _ in $(seq 1 50); do
  if curl --noproxy '*' --connect-timeout 2 --max-time 5 -fsS -o /dev/null \
      https://github.com/wouhliss/vgames/releases/latest/download/latest.json; then
    ready=true; break
  fi
  kill -0 "$server_job" 2>/dev/null || { wait "$server_job"; exit 1; }
  sleep 0.2
done
[[ "$ready" == true ]]
# Keep the published lower artifact intact: the updater replaces this private copy.
cp "$older" "$work/installed.AppImage"
chmod 700 "$work/installed.AppImage"
E2E_APP="$work/installed.AppImage" E2E_UPDATE_ARTIFACTS="$artifacts" \
APPIMAGE_EXTRACT_AND_RUN=1 "$here/../run.sh" 1,3
sudo python3 - "$artifacts/latest.json" "$work/requests.log" <<'PY'
import json, sys
from pathlib import Path
from urllib.parse import urlsplit
latest = json.loads(Path(sys.argv[1]).read_text())
requests = set(Path(sys.argv[2]).read_text().splitlines())
prefix = '/wouhliss/vgames/releases/latest/download/'
assert prefix + 'latest.json' in requests
assert prefix + 'changelog-user.json' in requests
assert urlsplit(latest['platforms']['linux-x86_64']['url']).path in requests
PY
