# Changelog fragments

**Every PR adds at least one fragment**, written by the agent that made the change; humans do not
write changelog text. CI runs `cargo xtask changelog lint`. At release, the release-notes agent
(08-release §3.4) curates user fragments into the launcher's "What's new".
Full rules: [docs/architecture/08-release.md §3](../docs/architecture/08-release.md).

```markdown
---
audience: user          # user | internal
component: launcher     # launcher | admin | server
type: added             # added | changed | fixed | removed | security
---
You can now pin favorite packages to the top of your library.
```

- File name: `<short-kebab-slug>.md` (unique; e.g. `pin-favorites.md`).
- `audience: user` text is shown **verbatim to players** in the launcher's "What's new".
  Write one or two plain sentences about what they can see or do. No technical words, code,
  paths, issue numbers or internals: the linter rejects them.
- Everything else (refactors, tests, CI, dependencies, invisible performance work) is
  `audience: internal`. It is still required, so every change is accounted for.
