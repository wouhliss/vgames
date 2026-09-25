---
audience: internal
component: server
type: added
---
Server image and scheduled pipelines: apps/api/Dockerfile (admin web + API, distroless, non-root, read-only friendly, base images pinned by digest), release-api.yml (multi-arch ghcr image signed with cosign keyless, SBOM and provenance attestations), nightly e2e.yml (CLI key pipeline and admin Playwright suite against a real API), soak.yml skeleton for a self-hosted runner, and grouped weekly Dependabot updates with crypto crates kept separate.
