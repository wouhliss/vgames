---
audience: internal
component: launcher
type: added
---
INS-07: `.github/workflows/transfer-loop.yml` runs the download retry test 200 consecutive times in debug and release (`scripts/ci/transfer-loop.sh`); its intermittent failure (Q13) was the old test shape, fixed in `7e111a0`.
