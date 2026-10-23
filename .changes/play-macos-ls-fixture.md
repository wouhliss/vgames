---
audience: internal
component: launcher
type: fixed
---
The native macOS launch-link registration test builds its bundle under the home folder: Launch Services flags bundles in `$TMPDIR` as `in-temp-dir` and never lists them as URL handlers (hosted diagnostic run 39952576812).
