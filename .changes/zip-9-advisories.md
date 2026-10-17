---
audience: internal
component: launcher
type: security
---
`zip` advisories RUSTSEC-2026-0367/0368/0369 (unsafe `extract()` on untrusted archives) failed the Supply chain check on `main`. The launcher's runtime extractor moves to zip 9 (`name()` is now fallible; a non-UTF-8 entry name refuses the archive as corrupt). `tauri-plugin-updater` drops its default `zip` feature, which only unpacks the legacy `.msi.zip`/`.nsis.zip`/`.AppImage.tar.gz` bundles we never publish, removing zip 4 from the launcher. The remaining zip 8.6 is a build dependency of `utoipa-swagger-ui` whose build script never calls `extract()`; `deny.toml` and `.cargo/audit.toml` record the exception and a `[bans]` rule confines zip < 9 to that wrapper. No code path ever called the affected `extract()` functions.
