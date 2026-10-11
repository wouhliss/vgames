#!/usr/bin/env bash
set -euo pipefail
args=(--ci --target "$TARGET" --bundles "$BUNDLES")
if [ -n "${SIGN_CONFIG:-}" ]; then args+=(--config "$SIGN_CONFIG"); fi
if [ -n "${BUILD_CONFIG:-}" ]; then args+=(--config "$BUILD_CONFIG"); fi
pnpm --filter @vgames/desktop tauri build "${args[@]}"
