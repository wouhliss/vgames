#!/usr/bin/env bash
# Throwaway macOS diagnostic for PR #273: which bundle property makes Launch
# Services list a freshly registered URL handler? Never merged.
set -uo pipefail
LSR=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
sw_vers
swiftc -O -o /tmp/handlers diag/handlers.swift
printf 'int main(void){return 0;}\n' > /tmp/fixture.c
cc -o /tmp/fresh /tmp/fixture.c

make_bundle() { # dir exe-source sign
  local dir=$1 src=$2 sign=$3 n=$RANDOM$RANDOM
  local id=app.vgames.diag.$n scheme=vgames-diag-$n app=$dir/Fixture$n.app
  mkdir -p "$app/Contents/MacOS"
  cp "$src" "$app/Contents/MacOS/fixture"
  cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>$id</string>
<key>CFBundleExecutable</key><string>fixture</string>
<key>CFBundleName</key><string>vgames protocol fixture</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>1</string>
<key>CFBundleURLTypes</key><array><dict>
<key>CFBundleURLName</key><string>$id</string>
<key>CFBundleTypeRole</key><string>Editor</string>
<key>CFBundleURLSchemes</key><array><string>$scheme</string></array>
</dict></array>
</dict></plist>
EOF
  [ "$sign" = adhoc ] && codesign --force --deep -s - "$app" 2>&1
  "$LSR" -f "$app"; echo "lsregister -f rc=$?"
  for i in 0 1 2; do /tmp/handlers "$scheme" "$id" && { echo "RESULT registered after ${i}s"; break; }; sleep 1; done
  "$LSR" -dump 2>/dev/null | grep -n -B2 -A40 "$id" | grep -iE "path:|flags:|claim|bindings|$scheme|identifier|apptype|executable" | head -25
  "$LSR" -u "$app"
}

TMPBASE=$(mktemp -d)
mkdir -p "$HOME/vgames-diag"
for loc in "$TMPBASE" "$HOME/vgames-diag"; do
  for exe in /usr/bin/true /tmp/fresh; do
    for sign in none adhoc; do
      echo "=================== loc=$loc exe=$exe sign=$sign"
      make_bundle "$loc" "$exe" "$sign"
    done
  done
done
