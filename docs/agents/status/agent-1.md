# Agent 1 status

Backend & DB Architect (`apps/api`, `crates/vgames-proto` minus social/realtime, non-social migrations).

## Done

## In progress
- A1-T01 — Service skeleton

## Interfaces delivered (other agents may now rely on these)

## Needs from others
- From Agent 5: `vgames_core` fingerprint (`VG1-…`) and key id functions (A5-T03) for `/.well-known/vgames.json`;
  `verify_bundle` / `verify_manifest` / `verify_compat_profile` (A5-T04) for trust and publishing endpoints.
- From Agent 2: `vgames_pack::verify::PackStreamVerifier` (A2-T03) for the `version.verify` job (A1-T12).

## Blockers / contract questions

## Local environment notes
- My tests use a dedicated Postgres 18 container on `127.0.0.1:55432` (`vgames-a1-pg`); ports 8080 and 4443
  are taken by other projects on this machine, so the dev API binds `127.0.0.1:8088` in my `.env`.
