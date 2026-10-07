#!/usr/bin/env bash
# Publishes the dummy game the M2 scenario installs (INS-09), the way an owner would: the
# `vgames` CLI signs in (fake Discord), uploads trust bundle v1 holding a publisher key,
# creates the package and publishes a signed linux-x86_64 release.
#
#   publish.sh <dir> <server>        # <dir> as written by api.sh up
#
# The game (bin/game) records each start in $HOME/.vgames-e2e-runs and then waits, so the
# suite can see it run and stop it. Test passphrases only (scripts/e2e/key-pipeline.sh).
set -euo pipefail
dir=${1:?work directory}
server=${2:?server url}
repo=$(cd "$(dirname "$0")/../../../.." && pwd)
target=${CARGO_TARGET_DIR:-$repo/target}
vgames=${VGAMES_BIN:-$target/debug/vgames}
# The system trust store (tls.sh added the throwaway CA there), not a CI or sandbox override.
unset SSL_CERT_FILE SSL_CERT_DIR CURL_CA_BUNDLE
export ROOT_PASS='e2e root passphrase: test only 1!' PUB_PASS='e2e publisher passphrase: test only 2!'
export VGAMES_CREDENTIAL_STORE=file VGAMES_CONFIG_DIR="$dir/config"
keys="$dir/keys"
field() { "$vgames" keys show "$1" | awk -v f="$2" '$0 ~ f { print $NF }'; }

info=$(curl -sSf "$server/.well-known/vgames.json")
server_id=$(jq -r .server_id <<< "$info")
fingerprint=$(jq -r .root_key_fingerprint <<< "$info")
"$vgames" login --server "$server" --fingerprint "$fingerprint" \
  --authorize-with "$repo/scripts/e2e/fake-discord-browser.sh" > /dev/null
holder=$(jq -r .user_id "$dir"/config/session-*.json)

{
  echo "server_id = \"$server_id\""
  echo '[[publishers]]'
  echo "public_key = \"$(field "$keys/old.vgkey" "Public key:")\""
  echo 'label = "e2e"'
  echo "holder_user_id = \"$holder\""
  echo 'not_before = "2026-01-01T00:00:00Z"'
  echo 'not_after = "2036-01-01T00:00:00Z"'
} > "$dir/v1.toml"
"$vgames" trust build --spec "$dir/v1.toml" --root "$keys/root.vgkey" --out "$dir/v1.json" --force > /dev/null
"$vgames" trust sign --bundle "$dir/v1.json" --root "$keys/root.vgkey" --out "$dir/v1.signed.json" \
  --yes --force --passphrase-env ROOT_PASS > /dev/null
"$vgames" trust publish --server "$server" --signed "$dir/v1.signed.json" > /dev/null

token=$(jq -r .access_token "$dir"/config/session-*.json)
curl -sSf -H "Authorization: Bearer $token" -H "Content-Type: application/json" \
  -H "Idempotency-Key: e2e-real-package-$(date +%s%N)" \
  -d '{"title":"E2E Game","slug":"e2e-game","fetch_metadata":false}' "$server/v1/admin/packages" > /dev/null
game="$dir/game"
mkdir -p "$game/bin"
cat > "$game/bin/game" <<'GAME'
#!/bin/sh
date +%s >> "$HOME/.vgames-e2e-runs"
exec sleep 600
GAME
chmod +x "$game/bin/game"
head -c 3000000 /dev/urandom > "$game/data.bin"
printf '[launch]\ndefault = "game"\n[[launch.targets]]\nid = "game"\nlabel = "Play"\nexecutable = "bin/game"\n' \
  > "$dir/launch.toml"
"$vgames" publish "$game" --server "$server" --package e2e-game --platform linux-x86_64 \
  --version-label 1.0 --key "$keys/old.vgkey" --execution "$dir/launch.toml" --publish --json \
  --passphrase-env PUB_PASS > "$dir/published.json"
[ "$(jq -r .state "$dir/published.json")" = published ]

# Visible in the catalog: packages start as drafts (JSON merge patch with the current ETag).
package=$(jq -r .package_id "$dir/published.json")
etag=$(curl -sSf -D - -o /dev/null -H "Authorization: Bearer $token" "$server/v1/admin/packages/$package" \
  | tr -d '\r' | awk 'tolower($1) == "etag:" { print $2 }')
curl -sSf -o /dev/null -X PATCH -H "Authorization: Bearer $token" -H "If-Match: $etag" \
  -H "Content-Type: application/merge-patch+json" -d '{"status":"published"}' \
  "$server/v1/admin/packages/$package"
