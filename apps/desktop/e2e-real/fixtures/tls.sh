#!/usr/bin/env bash
# A throwaway CA trusted by this machine, and stunnel terminating TLS for the API (INS-09).
# Release launchers refuse plain http even on loopback, and that check must never be
# relaxed: the harness serves the API on https://localhost instead.
#
#   tls.sh up <dir> <https-port> <api-port>   # CA + cert, trust the CA, start stunnel
#   tls.sh down <dir>                          # stop stunnel, untrust the CA
#
# Needs sudo (update-ca-certificates). CI runners and cloud sessions have it; never run
# this on a machine you care about without `down` afterwards.
set -euo pipefail
cmd=${1:?up|down}
dir=${2:?work directory}
ca_name=vgames-e2e-ca.crt
sudo=""
[[ $(id -u) -ne 0 ]] && sudo=sudo

case "$cmd" in
up)
  https_port=${3:?https port}
  api_port=${4:?api port}
  mkdir -p "$dir"
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 2 \
    -subj "/CN=vgames e2e CA (throwaway)" \
    -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign" \
    -keyout "$dir/ca.key" -out "$dir/ca.crt" 2>/dev/null
  openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj "/CN=localhost" \
    -keyout "$dir/server.key" -out "$dir/server.csr" 2>/dev/null
  openssl x509 -req -in "$dir/server.csr" -CA "$dir/ca.crt" -CAkey "$dir/ca.key" \
    -CAcreateserial -days 2 -out "$dir/server.crt" \
    -extfile <(printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n') 2>/dev/null
  rm -f "$dir/ca.key" "$dir/server.csr"
  $sudo cp "$dir/ca.crt" "/usr/local/share/ca-certificates/$ca_name"
  $sudo update-ca-certificates >/dev/null 2>&1
  cat > "$dir/stunnel.conf" <<CONF
foreground = no
pid = $dir/stunnel.pid
output = $dir/stunnel.log
[api]
accept = 127.0.0.1:$https_port
connect = 127.0.0.1:$api_port
cert = $dir/server.crt
key = $dir/server.key
CONF
  stunnel "$dir/stunnel.conf"
  for _ in $(seq 1 50); do
    curl -sf -o /dev/null --cacert "$dir/ca.crt" "https://localhost:$https_port/v1/health" && exit 0
    sleep 0.2
  done
  echo "stunnel is not answering on https://localhost:$https_port" >&2
  cat "$dir/stunnel.log" >&2 || true
  exit 1
  ;;
down)
  [[ -f "$dir/stunnel.pid" ]] && kill "$(cat "$dir/stunnel.pid")" 2>/dev/null || true
  $sudo rm -f "/usr/local/share/ca-certificates/$ca_name"
  $sudo update-ca-certificates >/dev/null 2>&1
  ;;
*)
  echo "usage: tls.sh up <dir> <https-port> <api-port> | down <dir>" >&2
  exit 2
  ;;
esac
