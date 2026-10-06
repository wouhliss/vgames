---
audience: internal
component: admin
type: added
---
A3-T18: forms matrix (create, editor, settings, allowlist × 409/412/428/429/500/offline, double submits, 500-code-point mixed-script input) and e2e/robustness.spec.ts (deep links to every package section and filter URL, back/forward, keyboard-only create/edit/filter with a focus-ring check on every Tab stop, axe on every page under 500/429/403/offline via a new `faults.api` mock switch, and a session ending mid-form). Fixed: "Create package" could send a second request between success and navigation.
