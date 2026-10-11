#!/usr/bin/env bash
set -euo pipefail
bundle="target/$TARGET/release/bundle"
out="out/$TARGET"
mkdir -p "$out"
case "$TARGET" in
  *windows*) cp "$bundle"/nsis/*-setup.exe "$out/"
             updater=("$out"/*-setup.exe) ;;
  *linux*)   cp "$bundle"/appimage/*.AppImage "$bundle"/deb/*.deb "$bundle"/rpm/*.rpm "$out/"
             updater=("$out"/*.AppImage "$out"/*.deb "$out"/*.rpm) ;;
  *darwin*)  cp "$bundle"/macos/*.app.tar.gz "$bundle"/dmg/*.dmg "$out/"
             updater=("$out"/*.app.tar.gz) ;;
esac
# Replaces the bundler's .sig: the trusted comment must carry version:<V>.
cargo xtask updater sign --version "$VERSION" "${updater[@]}"
