#!/usr/bin/env bash
# One isolated launcher instance behind tauri-driver (INS-09):
#
#   instance.sh <dir> <port>    # runs in the foreground until killed
#
# Everything a release launcher keys on is private to <dir>: HOME, XDG_{CONFIG,DATA,CACHE,
# STATE}_HOME, XDG_RUNTIME_DIR and the D-Bus session (dbus-run-session). The single-instance
# lock is a session-bus name and the keychain is the session's Secret Service (none here, so
# the launcher falls back to its file vault), so two instances on one bus would merge into
# one. The browser is fixtures/xdg-open. Needs DISPLAY (Xvfb), tauri-driver and
# WebKitWebDriver on PATH.
set -euo pipefail
dir=${1:?instance directory}
port=${2:?tauri-driver port}
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$dir"/{home,config,data,cache,state,runtime,bin}
chmod 700 "$dir/runtime"
cp "$here/xdg-open" "$dir/bin/xdg-open"
export VGAMES_E2E_REPO
VGAMES_E2E_REPO=$(cd "$here/../../../.." && pwd)
# The system trust store, as on a player's machine (tls.sh adds the throwaway CA there):
# SSL_CERT_FILE/SSL_CERT_DIR from a CI or sandbox environment would replace it.
exec dbus-run-session -- env -u SSL_CERT_FILE -u SSL_CERT_DIR \
  HOME="$dir/home" \
  XDG_CONFIG_HOME="$dir/config" XDG_DATA_HOME="$dir/data" \
  XDG_CACHE_HOME="$dir/cache" XDG_STATE_HOME="$dir/state" \
  XDG_RUNTIME_DIR="$dir/runtime" \
  PATH="$dir/bin:$PATH" \
  tauri-driver --port "$port" --native-port "$((port + 1))"
