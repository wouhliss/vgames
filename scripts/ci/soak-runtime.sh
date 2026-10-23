#!/usr/bin/env bash
# Precompiled soak artifacts need runtime libraries, not compiler/development packages.
set -euo pipefail
apt_options=(-o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30)
sudo timeout --signal=TERM --kill-after=30s 5m apt-get "${apt_options[@]}" -o APT::Update::Error-Mode=any update
sudo timeout --signal=TERM --kill-after=30s 15m apt-get "${apt_options[@]}" install -y --no-install-recommends \
  libwebkit2gtk-4.1-0 libgtk-3-0t64 libayatana-appindicator3-1 \
  librsvg2-2 libsoup-3.0-0 libjavascriptcoregtk-4.1-0 libdbus-1-3 libudev1 \
  libxss1 libxcursor1 libxi6 libxfixes3 libxrandr2 libxinerama1 \
  libasound2t64 libpulse0 libwayland-client0 libxkbcommon0 libegl1 libgles2 \
  libdrm2 libgbm1 libdecor-0-0 libnss3 libnspr4 xvfb xauth dbus-x11
