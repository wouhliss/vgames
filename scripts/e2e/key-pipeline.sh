#!/usr/bin/env bash
# End-to-end test of the key signature pipeline against a real API (A5-T06):
#   key-pipeline.sh init <dir>          root key + two publisher keys; prints the root public key
#   key-pipeline.sh run <dir> <server>  login (fake Discord) → bundle v1 → a package published
#                                       with the first publisher key, verified as launchers do →
#                                       bundle v2 revoking that key → launchers refuse the release
#                                       → replay of v1 refused → re-sign with the new key →
#                                       launchers accept the release again
# The API must run with VGAMES_ROOT_PUBLIC_KEY=<printed key>, VGAMES_DEV_FAKE_DISCORD=true and
# VGAMES_BOOTSTRAP_OWNER_DISCORD_ID=100000000000000001. VGAMES_BIN points at the `vgames` binary.
# Test passphrases only: never reuse this with real keys.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
vgames=${VGAMES_BIN:-vgames}
export ROOT_PASS='e2e root passphrase: test only 1!' PUB_PASS='e2e publisher passphrase: test only 2!'

field() { "$vgames" keys show "$1" | awk -v f="$2" '$0 ~ f { print $NF }'; }

init() {
  local dir=$1
  mkdir -p "$dir"
  "$vgames" keys init-root --out "$dir/root.vgkey" --label e2e-root --passphrase-env ROOT_PASS > /dev/null
  "$vgames" keys issue-publisher --out "$dir/old.vgkey" --label e2e@old --passphrase-env PUB_PASS > /dev/null
  "$vgames" keys issue-publisher --out "$dir/new.vgkey" --label e2e@new --passphrase-env PUB_PASS > /dev/null
  field "$dir/root.vgkey" "Public key:"
}

spec() { # spec <out> <server_id> <holder> <publisher key file> [revoked key id]
  {
    echo "server_id = \"$2\""
    echo '[[publishers]]'
    echo "public_key = \"$(field "$4" "Public key:")\""
    echo 'label = "e2e"'
    echo "holder_user_id = \"$3\""
    echo 'not_before = "2026-01-01T00:00:00Z"'
    echo 'not_after = "2036-01-01T00:00:00Z"'
    if [ -n "${5:-}" ]; then
      echo '[[revoked]]'
      echo "key_id = \"$5\""
      echo 'reason = "e2e rotation"'
    fi
  } > "$1"
}

run() {
  local dir=$1 server=$2
  export VGAMES_CREDENTIAL_STORE=file VGAMES_CONFIG_DIR="$dir/config"
  local info server_id fingerprint holder old_id
  info=$(curl -sSf "$server/.well-known/vgames.json")
  server_id=$(jq -r .server_id <<< "$info")
  fingerprint=$(jq -r .root_key_fingerprint <<< "$info")
  [ "$fingerprint" = "$(field "$dir/root.vgkey" Fingerprint:)" ] || { echo "the server does not use the e2e root key" >&2; exit 1; }

  echo "== login (PKCE, pinned fingerprint)"
  "$vgames" login --server "$server" --fingerprint "$fingerprint" --authorize-with "$here/fake-discord-browser.sh"
  holder=$(jq -r .user_id "$dir"/config/session-*.json)

  echo "== trust bundle v1"
  spec "$dir/v1.toml" "$server_id" "$holder" "$dir/old.vgkey"
  "$vgames" trust build --spec "$dir/v1.toml" --root "$dir/root.vgkey" --out "$dir/v1.json" --force > /dev/null
  "$vgames" trust sign --bundle "$dir/v1.json" --root "$dir/root.vgkey" --out "$dir/v1.signed.json" --yes --force --passphrase-env ROOT_PASS > /dev/null
  "$vgames" trust publish --server "$server" --signed "$dir/v1.signed.json"
  [ "$(curl -sSf "$server/v1/trust/bundle" | jq -r .bundle | base64 -d | jq .version)" = 1 ]

  echo "== a package, published with the first publisher key"
  local token package
  token=$(jq -r .access_token "$dir"/config/session-*.json)
  package=$(curl -sSf -H "Authorization: Bearer $token" -H "Content-Type: application/json" \
    -H "Idempotency-Key: e2e-package-$(date +%s%N)" \
    -d '{"title":"E2E Game","slug":"e2e-game","fetch_metadata":false}' "$server/v1/admin/packages" | jq -r .id)
  mkdir -p "$dir/game/bin" "$dir/game/saves"
  printf '#!/bin/sh\necho e2e\n' > "$dir/game/bin/game"
  chmod +x "$dir/game/bin/game"
  head -c 3000000 /dev/urandom > "$dir/game/data.bin"
  printf '[launch]\ndefault = "game"\n[[launch.targets]]\nid = "game"\nlabel = "Play"\nexecutable = "bin/game"\n' > "$dir/launch.toml"
  "$vgames" publish "$dir/game" --server "$server" --package e2e-game --platform linux-x86_64 \
    --version-label 1.0 --key "$dir/old.vgkey" --execution "$dir/launch.toml" --publish --json \
    --passphrase-env PUB_PASS > "$dir/published.json"
  [ "$(jq -r .state "$dir/published.json")" = published ]
  [ "$(jq -r .package_id "$dir/published.json")" = "$package" ]
  "$vgames" verify --server "$server" --package e2e-game --platform linux-x86_64

  echo "== trust bundle v2 revokes the first publisher key"
  old_id=$(field "$dir/old.vgkey" "Key id:")
  spec "$dir/v2.toml" "$server_id" "$holder" "$dir/new.vgkey" "$old_id"
  "$vgames" trust build --spec "$dir/v2.toml" --root "$dir/root.vgkey" --previous "$dir/v1.signed.json" --out "$dir/v2.json" --force > /dev/null
  "$vgames" trust sign --bundle "$dir/v2.json" --root "$dir/root.vgkey" --out "$dir/v2.signed.json" --yes --force --passphrase-env ROOT_PASS > /dev/null
  "$vgames" trust publish --server "$server" --signed "$dir/v2.signed.json"

  echo "== launchers refuse the release signed by the revoked key"
  if "$vgames" verify --server "$server" --package e2e-game --platform linux-x86_64 2> "$dir/verify.err"; then
    echo "the release still verifies under bundle v2" >&2; exit 1
  fi
  grep -qi "revoked" "$dir/verify.err"

  echo "== replaying v1 is refused"
  if "$vgames" trust publish --server "$server" --signed "$dir/v1.signed.json" 2> "$dir/replay.err"; then
    echo "v1 was accepted again" >&2; exit 1
  fi
  grep -q "rollback" "$dir/replay.err"

  echo "== re-sign: the dry run lists the release, then it is re-signed with the new key"
  "$vgames" trust re-sign --server "$server" --from "$old_id" --key "$dir/new.vgkey" --previous "$dir/v1.signed.json" --dry-run \
    | grep -q "$(jq -r .id "$dir/published.json")"
  "$vgames" trust re-sign --server "$server" --from "$old_id" --key "$dir/new.vgkey" --previous "$dir/v1.signed.json" \
    --yes --passphrase-env PUB_PASS

  echo "== launchers accept the re-signed release under bundle v2"
  "$vgames" verify --server "$server" --package e2e-game --platform linux-x86_64 --json > "$dir/verified.json"
  [ "$(jq -r .key_id "$dir/verified.json")" = "$(field "$dir/new.vgkey" "Key id:")" ]
  [ "$(jq -r .trust_bundle_version "$dir/verified.json")" = 2 ]

  echo "== logout"
  "$vgames" logout --server "$server"
  echo "key pipeline: OK"
}

case "${1:-}" in
  init) init "$2" ;;
  run) run "$2" "$3" ;;
  *) echo "usage: $0 init <dir> | run <dir> <server>" >&2; exit 2 ;;
esac
