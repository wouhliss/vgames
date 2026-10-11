#!/usr/bin/env bash
set -euo pipefail
conf=apps/desktop/src-tauri/tauri.conf.json
case "$(jq -r .plugins.updater.pubkey "$conf")" in
  REPLACE_*|""|null) echo "::error::set plugins.updater.pubkey (docs/security/release.md)"; exit 1;;
esac
jq -e '.plugins.updater.requireSignedVersion == true' "$conf"
jq -e '.bundle.createUpdaterArtifacts == true' "$conf"
