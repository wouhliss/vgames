#!/bin/sh
# Stands in for the browser during `vgames login --authorize-with` against an API
# running with VGAMES_DEV_FAKE_DISCORD=true (debug builds, localhost only): signs in
# through the fake Discord page and prints the vgames:// callback link.
# Usage: fake-discord-browser.sh <sign-in URL>   (DISCORD_ID / DISCORD_NAME override the account)
set -eu
url="$1"
state=$(printf '%s\n' "$url" | sed -n 's/.*[?&]state=\([^&]*\).*/\1/p')
origin=$(printf '%s\n' "$url" | sed -n 's|^\(https\{0,1\}://[^/]*\).*|\1|p')
if [ -z "$state" ] || [ -z "$origin" ]; then
  echo "unexpected sign-in URL: $url" >&2
  exit 1
fi
callback=$(curl -sS -o /dev/null -w '%{redirect_url}' \
  "$origin/v1/auth/dev/fake-discord/submit?state=$state&id=${DISCORD_ID:-100000000000000001}&name=${DISCORD_NAME:-owner}")
curl -sS -o /dev/null -w '%{redirect_url}\n' "$callback"
