---
audience: internal
component: server
type: added
---
`scripts/ci/local.sh` runs every `ci.yml` job locally before a push (commit first; summary in
`target/ci-local/summary.md`). GitHub CI is unchanged; docs/security/ci.md lists the settings for the now-public
repository.
