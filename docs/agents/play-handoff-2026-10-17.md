# PLAY handoff — 2026-10-17

The user requested a clean stop and pushed work so another agent can continue.
This is a checkpoint, not completion of PLAY-01 through PLAY-11. No feature PR
was merged during this stop. Read `AGENTS.md`, `docs/agents/phase-2/README.md`
and `docs/agents/phase-2/play-launch-platform.md` before continuing.

## Start here

1. Fetch `origin`. This handoff starts at main
   `957aabe21514782c823ed9dbb532b295a012e901`. Feature branches with recent green
   checks below were based on older main `8923955`; their checks cannot authorize
   a merge on today's main. Rebase, push and wait for every check again.
2. Fix the native macOS registration failure in #273. The required role was
   missing and is now supplied, but the native failure remains. Do not report
   the role fix as a completed registration fix.
3. Integrate the controller stack in dependency order, keeping all main commands
   and generated bindings. Do not merge composite scratch bases.
4. Continue cloud-save durable backups and the restore journal before enabling
   restore writes, then the service, hooks, real-API scenarios and UI.

All existing local PLAY branch tips were audited against fetched remote refs.
The three previously unpublished branches `play/p2-controller-input`,
`play/p2-controller-launch-work` and `play/p2-library-fixture` were pushed under
their existing names. They preserve older/intermediate work, not new merge
candidates. All other 52 existing PLAY branches already matched origin exactly.
The inventory at the end records every existing PLAY tip; the new handoff branch
is additional. No local branch or worktree was deleted.

## Fresh hosted evidence checked on October 17

These results are tied to the exact recorded heads, not current main.

| PR | Branch suffix (`play/`) | Head prefix | CI run / result | Desktop matrix run / result |
| --- | --- | --- | --- | --- |
| #207 | p2-controller-routing | 50e2be9 | 38960213015 / pass | 38960213058 / pass |
| #208 | p2-controller-runtime | 3c56716 | 38960255182 / fail | 38960255240 / pass |
| #209 | p2-controller-events | 995b786 | 38960722624 / pass | 38960722626 / pass |
| #266 | p2-deeplink-registration | 513e2c7 | 38960208488 / pass | 38960208467 / pass |
| #273 | p2-deeplink-macos | 7af8edb | 38960329217 / pass | 38960329136 / fail |
| #277 | p2-link-refusals | 3dfe216 | 38960192444 / pass | 38960192425 / pass |
| #278 | p2-shortcut-icons | 3029443 | 38960205362 / pass | 38960205105 / pass |

Workflow links use `https://github.com/wouhliss/vgames/actions/runs/<run-id>`.
#208's failing job is Admin UI end-to-end (mock mode), job `117173221753`:
pnpm self-install failed with npm `ECONNRESET`, exit 152, before browser tests.
Its native three-platform matrix passed. This is infrastructure evidence, not
an application-test failure; rebase first rather than rerun a stale-base build.

### #273: unresolved native macOS failure

Run [38960329136](https://github.com/wouhliss/vgames/actions/runs/38960329136),
job [117175109744](https://github.com/wouhliss/vgames/actions/runs/38960329136/job/117175109744):
462 native tests passed; one failed; three were ignored by existing suite policy.
The failure is
`deeplink::registration::macos::tests::native::launch_services_restores_a_deleted_registration_for_a_repaired_bundle`,
at `apps/desktop/src-tauri/src/deeplink/registration/macos/tests/native.rs:116`:
`assert!(registered(&scheme, &id))`. It fails after the **first** repair, before
unregister/re-register. `lsregister -f` returned success, but
`LSCopyAllHandlersForURLScheme` did not contain the unique bundle identifier.
Linux and Windows legs passed.

The role repair has portable regression coverage: `CFBundleTypeRole=Editor` is
added for a new declaration; an incomplete existing declaration is repaired
without duplicates, preserving unrelated fields; signed incomplete bundles are
not rewritten. Apple documents the role as required in Core Foundation Keys,
CFBundleURLTypes table 7. This concrete defect did not explain the entire native
failure. The native assertion is unchanged; no wait, skip or timeout was added.

Investigate actual Launch Services registration and fixture validity. The fixture
creates a temporary `.app`, copies `/usr/bin/true` as its executable, supplies a
unique bundle ID and URL scheme and queries handlers through CoreServices.
Hidden temporary paths, executable identity/signature and registration visibility
are hypotheses only. Obtain evidence before changing behavior or test assumptions.
Keep unique-scheme cleanup; do not reset the user's entire registration database.

## Branch dependencies and remaining scope

### Controllers — PLAY-04 / PLAY-05

- #207 routing is the immediate parent of #208 SDL runtime and #209 typed events.
  #209 now passes 17 local command/binding/capability checks, five event tests,
  all 313 desktop UI tests, TypeScript, desktop lint, strict all-target Clippy and
  formatting. Bindings were generated, retaining all main commands.
- #213 startup is older and needs current #208 plus #209 integrated. #205 controller
  UI and #206 profiles UI also need refresh. #214 driver status, #215 Linux virtual
  backend, #216 Windows Xbox backend, #237 DS4 backend, #239 session book and #218
  macOS gate are older stacked slices; inspect each parent before rebasing.
- #243/#244/#246 and `p2-controller-launch-base` contain composite integration
  work, including hooks, virtual-input filtering and hotplug. Preserve the work
  but never merge their composite bases as though they were clean main branches.
  `p2-controller-launch-work` is another older composite snapshot.
- HidHide/EVIOCGRAB hiding, trusted re-attachment, visible fallback warnings and
  final whole-app integration remain unfinished. Do not claim hardware support
  from synthetic fixtures alone.
- Already recorded on main: #234 Linux uinput fixture, 1,000 input-to-virtual
  samples, p99 1,103 microseconds against 2,000; #236 four-hour run
  `38368870742`, 2,431,458 reports, p99 1,153 microseconds, constant 8,484 KiB RSS,
  five threads and 20 descriptors. Evidence is
  `docs/agents/evidence/play-controller-soak-2026-10-13.jsonl` (#247).
  This is a synthetic two-pad backend measurement, not whole-launcher budgets,
  physical hiding, real hardware or the required eight-hour run. Do not rerun the
  four-hour test merely because unrelated documentation changed.

### Cloud saves — PLAY-06

- Stack: #248 HTTP -> #250 scanner -> #270 restore policy. #254 SQLite state is
  separate. #257 blobs is on an older HTTP parent. #249 compatibility bases,
  #251 native bases and #245 pending IPC need integration; #251 is composite.
- HTTP and blob transport retain private staging, bounded streaming, digest/size
  checks, expiry, total deadlines, cancellation and URL-free errors. No restore
  or actual sync service is enabled by these slices.
- Scanner uses capability-relative no-follow traversal, bounded globs, hashes
  changed metadata only and rejects links, collisions and overlapping roots.
  Missing native bases are empty without creation; dangling bases are refused.
- Restore policy performs a read-only preflight of signed roots/filters, portable
  paths and tree/hash/count/size limits. Installed inventory protects **any**
  destination in an install or its ancestor. Native identity comparison normalizes
  NFC before case folding, covering macOS decomposed names. Signed relative-path
  validation remains strict. All 16 local save tests and strict Clippy passed on
  the recorded October 15 source base; revalidate after integration.
- #254 appends `0010_save_accounts` after existing migrations. It isolates account
  state, persists UUIDv7 operation IDs and exact retry payloads through reopen,
  rejects stale acknowledgements and atomically commits finished base/file state.
  Existing legacy rows remain inaccessible under an empty account. Renumber only
  this unmerged migration if main introduces a numbering collision. The previous
  run passed 39 database tests and strict Clippy.
- Next implementation: durable scoped backups and restore journal, recovery and
  cancellation, then actual pull/push service, launch hooks, decisions/conflicts,
  quota/missing-blob cases, five-backup history, real API and sequential cross-OS
  artifact roundtrip, and complete UI.
- G1 forbids unauthenticated server bytes anywhere inside install directories,
  not only overwriting signed package files. Install-local restore is currently
  refused. #242 proposes the contract resolution; do not silently enable writes.

### Deep links and shortcuts — PLAY-07

- #266 refreshes current-executable registration on Windows and Linux. Windows
  uses per-user registration. Linux writes an atomic desktop entry and invokes
  bounded/cancellable system tools; real GLib launch tests preserve spaces,
  quotes, dollars and percent signs as data. Deb/rpm declare desktop-file-utils
  and xdg-utils; AppImage prerequisites are documented. #273 is stacked on #266.
- #277 implements fixed-plaintext refusal notices with generated event types.
  Reasons are rate_limited/not_installed/not_verified/unknown_package. Only an
  authenticated selected-server 404 establishes unknown_package; offline or
  malformed results do not. Payloads contain no URL/path/server text. At most
  one notice waits for UI readiness, and the one-second limiter applies to actual
  publication. A real slow-startup regression failed before this fix. Evidence:
  11 native deep-link checks, 17 command checks, strict Clippy; 318 UI cases and
  four Chromium accessibility/keyboard scenarios on the recorded earlier source.
- #278 retains cached-cover icons outside the evictable image cache, with bounded
  decoding, PNG for Linux, ICO for Windows and the bundled icon as fallback.
  Windows LoadImageW, Linux desktop-file-validate and macOS plutil fixtures now
  all pass on the exact hosted head. The corrected test asserts each shortcut
  format's backslash escaping; the production renderer was already correct.
- The final bundled `.webloc`/application-handler smoke and integration remain.

### App commands, packaging, performance and handoff — PLAY-08 through PLAY-11

- #219 bounded native http(s) external links and #264 generated third-party
  notices remain open slices. #264 generates actual Rust/build and production
  npm license/copyright notices, embeds them at build time and exposes plain text
  through app_licenses. Metadata-only entries are explicit, never fabricated.
  It needs installed npm dependencies for native builds. Rebase these branches
  and obtain current hosted results; no fresh green claim is made here.
- Appearance (#241), diagnostics and earlier launch lifecycle/hook work are
  already on main. Main status records the original merge evidence.
- PLAY-09 actual bundles/install smoke, PLAY-10 whole-launcher release budgets
  and eight-hour validation, and final PLAY-11 acceptance remain unfinished.
  This stop document is not their acceptance sign-off.

## Contracts and merge discipline

- #196 controller notes, #242 cloud saves and #274 refusal events are contract
  proposals. Check INT's current activity and README section 3a before any author
  merge; do not assume inactivity. Security paths always require INT.
- Fresh fetch immediately before each merge. If main moved since the branch's
  last push, rebase and push, then wait for every check, including the full desktop
  matrix. Never use old green results after a rebase, even for docs-only changes.
- Preserve every other agent's append-only entry; main entries first, own entries
  last. Generated bindings are regenerated through
  `VGAMES_UPDATE_BINDINGS=1 cargo test -p vgames-desktop --lib commands:: --locked`.
  Never hand-edit generated bindings or silently resolve contract changes.
- Never merge failing #273 or skip/weaken its native assertion. No security path
  exception or direct main push is needed for this handoff.

## Local workspace and resumption

- Existing worktrees are under `/workspace/vgames*`; all share one git repository.
  No build/test sessions were left running by this handoff. Docker was not started
  for documentation-only work. Existing `vgames-play-build` can be restarted.
- Build environment: Ubuntu 24.04 Docker container, Rust/Node/pnpm and GTK/WebKit/
  SDL build prerequisites already installed. Read the cloud runtime Docker/network
  references before restarting it. Re-resolve the current session proxy and pass
  it into networked Docker commands; saved IPs from prior sessions are stale.
  Preserve CA verification. Never log credentials or the process environment.
- Shared target is `/workspace/vgames/target`, with `CARGO_HOME=/home/agent/.cargo`,
  `RUSTUP_HOME=/home/agent/.rustup`, cargo bin on PATH, `SQLX_OFFLINE=true`,
  `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`.
  Use `--locked --offline` where possible. Run cargo work sequentially and never
  rebase a worktree during its build. Disk was approximately 1 GiB free at stop.
- Full desktop UI command: `pnpm --filter @vgames/desktop exec vitest run --maxWorkers=2`.
  Chromium/Xvfb are installed inside the existing build container. Kernel-uinput
  proof belongs on hosted CI; local `/dev/uinput` was unavailable.
- `gh` authentication was unavailable; git fetch/push and GitHub connector tools
  worked. Prefer the configured connector to reconstruct workflow/job evidence.
- Before this stop, untracked files were only four dependency symlinks, generated
  permission files from shared-target builds and Python bytecode. All 18 were
  inspected and moved, without discarding source, to
  `/workspace/play-local-artifacts-2026-10-17/manifest.json`. Restore dependency
  symlinks if desired or install with the frozen lockfile. The symlinks pointed
  to `/workspace/vgames/node_modules` and its `apps/desktop/node_modules`.
  These are reproducible local artifacts and intentionally are not source commits.
- Historical workspace-only scratch journal: `/workspace/play-progress.md`;
  audit JSON: `/workspace/play-{branch,worktree}-audit.json`. Local verification
  logs live inside Docker `/tmp/vgames-*`, notably `icons-fix-*`, `mac-role-*`
  and `controller-events-current-*`. This document and remote branches suffice
  to resume even if the local environment and these scratch artifacts disappear.

## Preserved PLAY branch tips

The inventory below was generated from local refs after the push audit. A tip is
preserved work, not a claim that its branch is current, open or safe to merge.

| Branch (`play/`) | Full preserved SHA |
| --- | --- |
| `p2-app-licenses` | `61f6d1bd6ee8e1f23aacc486ae34035de60f1949` |
| `p2-appearance` | `c15a5a1a57848ebcb4674bfaaeefd6b52443b5ea` |
| `p2-controller-context` | `9c693b23185ce2de21adcc49a511888e343361e6` |
| `p2-controller-contract` | `0eb2ff35b03c0f571c8663b1cf76096b2e4ea922` |
| `p2-controller-drivers` | `7629b5b4d2fe31d40705594237bd0ce7ac2c3940` |
| `p2-controller-events` | `995b7860694c99470246a645ea7cd50f1d92a060` |
| `p2-controller-evidence` | `3f7c5be5ab15dd47529d18b8760bf7d9c3f88a1b` |
| `p2-controller-hotplug` | `22dff621dfb9b230163492dafbbe55c021d35328` |
| `p2-controller-idle` | `c81772fbeeb992901f3d24c1284fba2b47d248fa` |
| `p2-controller-input` | `75c486ad8c8c02c135e782f73e1f95bbeb5ed752` |
| `p2-controller-ipc` | `788ab81e600992dc5f3c9fcd6eb1b9cd6c833544` |
| `p2-controller-launch-base` | `88f68926555720a4a04b298b5fb380596fef416b` |
| `p2-controller-launch-hooks` | `52535baa3939548542581454ca52abd9d4bb8130` |
| `p2-controller-launch-work` | `847f7a7cd158beeef9ea3e4d5332a2b655740947` |
| `p2-controller-navigation` | `244a62dee1fab59c48f13f65bdae09ead5fbc9c7` |
| `p2-controller-profile-commands` | `bc74fbc1430506004e4e6df167fda55e58fd0ba3` |
| `p2-controller-profile-ui` | `5aec48e109d3d6a16e721a5a6f22bd412ad2c367` |
| `p2-controller-profiles` | `807f715f114718f532946358142ea7481e9d418b` |
| `p2-controller-routing` | `50e2be9c1ea708c050be8d01b0c3b59cf1af5514` |
| `p2-controller-runtime` | `3c56716077d60c642173fb05584f5c5a58e78315` |
| `p2-controller-service` | `e56334873fec1f60c11bca5f49bc30276fe4aeff` |
| `p2-controller-sessions` | `aaccfb3620877f46ab088f149b84cf25af81e7fd` |
| `p2-controller-soak` | `5881be61a0843b6b04853f9b97604283d2c2cf47` |
| `p2-controller-startup` | `2fdf6076815b4228589413790330de9fbdc9fc3a` |
| `p2-controller-virtual-filter` | `d00654aca143106a05e462d683e68118add1d8bd` |
| `p2-controllers-ui` | `d75279175d70326e9d69e03ea8d796cf3eb2d092` |
| `p2-deeplink-macos` | `7af8edb519438de385d9220f7609516150f476d6` |
| `p2-deeplink-registration` | `513e2c70e8647e1ecef07bfdfd8f4ec8feea43e5` |
| `p2-diagnostics` | `d667bf9844475afa04fd8a8d15b22bd4ef894274` |
| `p2-external-links` | `0feea859d010c79d35d8ce9212441fa7b68e608d` |
| `p2-hook-child-fixture` | `b9dcd9b41d56d9e4a90c5f9afc24e030cb8af2df` |
| `p2-idle-thread` | `3974e046d53c0401208afc987556d30621ece2cc` |
| `p2-launch-hooks` | `4d62ef6ef009055f3edccace276d1d319e6e0379` |
| `p2-library-fixture` | `6dec838707c0a78ae6369ea1c9be0cb88c86937e` |
| `p2-link-refusal-contract` | `dd512f7dce912a43ef7807fbb73696e0f73a1721` |
| `p2-link-refusals` | `3dfe2162e4b64f9fa08f146b0e2994ecb7b06645` |
| `p2-macos` | `c12e2df2b07038c7134c492cb344262ef3b2df2c` |
| `p2-save-bases` | `03464657566d284e9c6a725070a424ada17a8146` |
| `p2-saves-blobs` | `375f202d682d4d6591da33bb43c9aa4e3971be66` |
| `p2-saves-contract` | `08275705c7f86eff1ffa6e6bd0c31beb141a8ca7` |
| `p2-saves-http` | `c7722e7a354a8b0f98446786a4bdc6310cd702b3` |
| `p2-saves-pending` | `976614f84bca186818d22010fdc25d971aa66435` |
| `p2-saves-platform` | `a5b97cc031ce8ac188bee93e419dd1fa99c89580` |
| `p2-saves-platform-base` | `60ba1ece6db5961cce98f78c3e13187e5de0882a` |
| `p2-saves-restore-policy` | `3bd51c7624b755697d149fd4e89da09253634867` |
| `p2-saves-scan` | `6f6d9c3713c06de0ca84f5d5cd555d74d1e738c1` |
| `p2-saves-state` | `97ecb1318829da050f4d29d3efdf24760a40a847` |
| `p2-sdl-static` | `da65745f459adcdfe7c224183157e0e3a01bb919` |
| `p2-shortcut-icons` | `3029443bbc294eadd645b30a0a0742b764543484` |
| `p2-uinput-kernel` | `776637b7bb56365679630daf3ba91611adb259cb` |
| `p2-virtual-ds4` | `9d9e1db283959c36417a9ab639f06e5c3ae1db66` |
| `p2-virtual-linux` | `bda2b20c36536aa8f648fa9b3c9acaa01bdd1613` |
| `p2-virtual-macos` | `2acb4ad93b07ec56327db8cf31bee2fdb6792e23` |
| `p2-virtual-windows` | `d766517aa1f95ec8aba5301ecca6da1781f4b6b4` |
| `p2-windows` | `706a44cf361c8cef1079141e50b85dd1f47d04d4` |
