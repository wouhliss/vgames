# Launch-link refusal notices (PLAY-07)

Additive launcher event contract for the four reasons required by PLAY-07.
The native router emits `deeplink-refused` with exactly `{reason}`. `reason` is
`rate_limited | not_installed | not_verified | unknown_package`. The event contains
no link, arguments, package identity, server text, filesystem path or credentials.

- `rate_limited`: the normal launcher rate limit refused the request.
- `not_installed`: no local installed package exists. A failed/offline catalog lookup
  must not be presented as evidence that the package is unknown.
- `not_verified`: signed-package validation refused the install, including incomplete
  installs, integrity failures and revoked signing keys.
- `unknown_package`: the selected server's authenticated package lookup returned 404,
  or no active server exists in which the launch-link package could be resolved.

The router retains all normal launch verification and rate limits. It never installs
or verifies automatically, changes arguments, or retries a launch in response to a
notice. Other launch failures retain their existing behavior; they are not mislabeled
as one of these four reasons.

Native notices wait for UI readiness, stop on shutdown, and are limited to one notice
per second across reasons. Catalog classification has a bounded deadline and uses the
server selected for the attempted launch, even if the active server changes later.
The app shell shows fixed plain-text messages in its existing accessible toast region;
server text and the original link never become notice content.

Required evidence: native event payload/throttle tests, unknown-vs-offline classification,
all four UI messages, keyboard dismissal, and an accessibility check. The no-panic parser
property test remains unchanged. Native generated event bindings replace any pending entry.
