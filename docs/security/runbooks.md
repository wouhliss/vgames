# Security runbooks

Owner: Agent 5 (A5-T13). What to do, step by step, with each key and in each incident this project plans for.
Every `vgames` command here exists today. Background: [01-security](../architecture/01-security.md) (trust chain, §3),
[release.md](release.md) (release keys), [test-matrix.md](test-matrix.md) (what is tested).

**Keep an incident log** from the first minute: time (UTC), who, what was decided, what was run. The steps below
say "record T*n*" where a time matters later (communication, post-mortem, drill timing).

## 0. Keys and secrets at a glance

| Secret | Lives | Held by | Someone who has it can… | …but not | Runbook |
|---|---|---|---|---|---|
| **Server root key** (`vgames.key/1`, Argon2id + XChaCha20-Poly1305) | Offline machine + two offline copies | Server owner | Sign trust bundles: trust any publisher key, announce a `next_root` | Reach launchers except through the pinned server over TLS | §1, §4, §5 |
| **Publisher key** (`vgames.key/1`) | One admin's machine; in the browser only inside the signing Worker while unlocked | That admin | Sign manifests that launchers accept until the key is revoked | Upload through this server without the holder's account (finalize requires `holder_user_id == caller`) | §2, §3 |
| **Updater key** (minisign) | `release` environment secrets `TAURI_SIGNING_PRIVATE_KEY[_PASSWORD]`; offline backup | Maintainers | Sign launcher builds that installed launchers accept as updates | Deliver them without publishing a release of this repository (`releases/latest/download/latest.json`) | §6 |
| **Runtime-catalog key** (minisign) | `release` environment secrets `VGAMES_RUNTIME_CATALOG_KEY[_PASSWORD]`; offline backup | Maintainers | Sign a catalog pinning any runtime archive (code that runs Windows games on Linux/macOS) | Deliver it without publishing to this repository's `runtimes` release | §7 |
| **OS code signing** (Authenticode certificate, Apple Developer ID) | `release` environment secrets `WINDOWS_CERTIFICATE*`, `APPLE_*` | Maintainers | Sign any binary as vgames (antivirus reputation, Gatekeeper) | Pass the updater's minisign check | §6 |
| **Server secrets**: `DISCORD_CLIENT_SECRET`, database roles, GCS service account, `VGAMES_FS_URL_SIGNING_KEY`, `VGAMES_SERVER_SECRET` | The server's secret store | Server operator | Impersonate the app to Discord, read and write the database and storage, sign storage links, forge page cursors | Sign trust bundles, manifests, updates or catalogs | §8 |
| **Player tokens** (access 15 min, refresh 30 days sliding) | OS keychain (0600 file fallback) | Each player | Act as that player on that server until revoked | Anything on another server | §8, §9 |

## 1. Root key ceremony (new server)

Owner, once per server. Budget an hour. A second person as witness is good practice.

**Prepare:** an offline machine (a live USB system with networking disabled is enough); the `vgames` CLI binary,
checked against the release's `SHA256SUMS` on a networked machine first; two removable media; a passphrase of at
least six random words, written down and kept as carefully as the media (a safe, or a second person's password
manager).

1. On the offline machine:
   ```sh
   vgames keys init-root --out root.vgkey --label "<server name> root <YYYY-MM-DD>"   # asks for the passphrase twice
   vgames keys show root.vgkey                                                        # no passphrase needed
   ```
   Write the **fingerprint** (`VG1-…`) and the **public key** on paper. Copy `root.vgkey` to both media and
   store them in two places. The root key never goes on a networked machine.
2. On the server, set `VGAMES_ROOT_PUBLIC_KEY=<public key>` and restart. Check that
   `curl -s https://<server>/.well-known/vgames.json | jq -r .root_key_fingerprint` prints the fingerprint on paper.
3. Publish the fingerprint where players can compare it (your community's announcement channel or website), and
   share `vgames://server/add?url=https://<server>&fp=<fingerprint>` links: the launcher pins the root from the
   link instead of asking the player to compare.
4. First trust bundle, with your own publisher key (§2 for the key itself). On a networked machine:
   `vgames login --server https://<server>` (compare the fingerprint it shows), then write `trust.toml`
   (`vgames trust build --help` shows the format; `server_id` is in `/.well-known/vgames.json`, your user id in
   the admin UI's user list) and build it:
   ```sh
   vgames trust build --spec trust.toml --root <root public key> --out v1.json
   ```
   Carry `v1.json` to the offline machine, review and sign it, carry the result back and publish it:
   ```sh
   vgames trust sign --bundle v1.json --root root.vgkey --out v1.signed.json      # offline; shows what it signs
   vgames trust publish --server https://<server> --signed v1.signed.json         # online; verified locally first
   ```
5. Keep every signed bundle (`v1.signed.json`, `v2.signed.json`, …): the next one is built with `--previous`, and
   re-signing needs the last bundle that still lists a revoked key.

## 2. Issuing a publisher key

For each admin, on each machine they sign from (never copy a key file between machines).

1. The admin, on their machine: `vgames keys issue-publisher --out <name>@<machine>.vgkey --label <name>@<machine>`
   (asks for a passphrase), then `vgames keys show <file>`. They send the owner the **public key** and **key id**,
   never the file.
2. The owner confirms the key id with the admin over a second channel (a call, reading it out), and adds to
   `trust.toml`:
   ```toml
   [[publishers]]
   public_key = "<public key>"
   label = "<name>@<machine>"
   holder_user_id = "<the admin's user id>"   # admin UI → Users
   not_before = "<today>T00:00:00Z"
   not_after = "<at most two years later>T00:00:00Z"
   ```
3. Build with `--previous <current signed bundle>`, sign offline, publish (§1 step 4).
4. Check: the admin UI's publisher key list (`GET /v1/admin/trust/publisher-keys`) shows the key. `vgames publish`
   checks the key against the server's verified bundle before uploading anything, so the admin's first publish
   confirms it end to end.

`not_after` only stops the **server** from accepting new uploads signed by the key; launchers keep trusting what
it signed. Renew before it lapses: issue a new key (steps 1–3), then `vgames trust re-sign` (§3 step 5) if
releases should move to it.

## 3. Revoking a publisher key

When a machine is lost or stolen, an admin leaves, a key file or passphrase may have been exposed, or a
signature looks wrong. Revocation is the only thing that makes launchers distrust a key; there is no undo.

1. Record T0. Find the key id (admin UI publisher keys, or `vgames keys show`) and, if the key may have been
   stolen, the earliest time it could have been.
2. If the holder's **account** may be compromised too, do §9 step 1 first: disabling the account stops new
   uploads under that key immediately.
3. Make sure a replacement key exists (§2 steps 1–2; it can go in the same bundle).
4. Add to `trust.toml` and drop the key's `[[publishers]]` entry:
   ```toml
   [[revoked]]
   key_id = "<32 hex>"
   reason = "<short reason>"      # revoked_at defaults to now
   ```
   Build with `--previous`, sign offline, publish. Record T1. From now on the server refuses uploads signed by
   the key, and `vgames verify` refuses its releases ("signing key … has been revoked"). Launchers apply the
   revocation when they next fetch the bundle: at start, when switching to the server, or when the server
   reports a signing key they do not know. A launch of a game installed from such a release is blocked with a
   "Verify" action.
5. Re-sign what the key signed with the replacement key, dry run first:
   ```sh
   vgames trust re-sign --server https://<server> --from <old key id> --key <new>.vgkey --previous <last bundle listing the old key> --dry-run
   vgames trust re-sign --server https://<server> --from <old key id> --key <new>.vgkey --previous <…> --yes
   ```
   If the key was stolen, add `--finalized-before <T of theft>` and review every version finalized after it:
   yank those in the admin UI unless their publisher confirms them. Each signature is checked under the old key
   before it is replaced, and manifest bytes never change. Record T2.
6. For each package and platform: `vgames verify --server https://<server> --package <slug> --platform <platform>`
   prints `OK … signed by <new key id>`. Record T3.

**Known limits** (filed with the owning agents):
- `re-sign` reaches only versions that are the current release of their package and platform: the server does not
  yet expose older versions' signatures (contract request to Agent 1). Older versions stay uninstallable.
- The launcher's "Verify" action is meant to fetch the re-signed signature of an installed version (02 §9, §11), but
  its backend command is not implemented yet (finding for Agent 2). Until it is, players reinstall affected games.

### Revocation drill

Run it on a staging server once a quarter, and whenever the people holding the root change. `e2e.yml` runs the
same sequence every night against a fresh server (`scripts/e2e/key-pipeline.sh`): publish with key A, bundle
revoking A, launchers refuse, replay of the old bundle refused, re-sign with key B, launchers accept.

Machine time, measured on 2026-09-30 with a debug build and one 3 MB release:

| Step | Command | Time |
|---|---|---|
| Build, sign and publish the revoking bundle | `trust build`, `trust sign`, `trust publish` | 0.3 s |
| A launcher-side check refuses the release | `vgames verify` | 0.1 s |
| Dry run, then re-sign the release | `trust re-sign` | 0.4 s |
| A launcher-side check accepts it again | `vgames verify` | 0.1 s |

The machine time does not matter; the people do. Log T0–T3 in each drill. Targets: revocation published (T1)
within **1 hour** of the decision, releases re-signed (T2) within **4 hours**. If a drill misses them, find out
why (the offline machine was not at hand, a passphrase holder was away) and fix that.

## 4. Rotating the root key (planned)

The root is fine but should be replaced (a holder leaves, the media are old). `next_root` is the only rotation
path launchers know (01 §3.2).

1. Ceremony for the new root (§1 step 1). Keep the old root at hand.
2. Announce: build bundle N+1 with the **old** root and
   ```toml
   [next_root]
   public_key = "<new root public key>"
   ```
   Sign it with the old root, publish it. Launchers that fetch it will accept the new root.
3. Grace period: launchers learn about the new root only when they fetch that bundle (their next start). Wait at
   least **30 days**, longer for a server whose players start the launcher rarely.
4. Switch, both steps back to back:
   1. Set `VGAMES_ROOT_PUBLIC_KEY` to the new root and restart the server.
   2. Build bundle N+2 (`--root <new public key> --previous v<N+1>.signed.json`, no `[next_root]`), sign it with the
      **new** root offline, and publish it without `--root` from a CLI that was signed in before the switch: it
      learned the announced root when N+1 was published. (A CLI that signs in after the switch pins the new root
      and cannot verify N+1; there, publish with `--root <new public key> --new-identity`.)

   Why this order: if N+2 went out first, launchers fetching it would move their pin to the new root while the
   server still presents the old one, and block it as a fingerprint mismatch. In the minutes between the two
   steps, a player adding the server for the first time pins the new root but can only fetch N+1 (signed by the
   old one) and gets an error until N+2 is out.
5. Announce the new fingerprint (§11). Launchers that never fetched N+1 now see a changed root and block the
   server, with no bypass by design: their players remove the server and add it again with the new fingerprint.
6. After the switch, wipe the old root's media and note it in the log: a launcher still holding the N+1 pin would
   accept a bundle signed by it.

This section and §5 were run end to end against the API on 2026-09-30 (roots A → B announced and switched, then B
"lost" and replaced by C with `--new-identity`; replaying a bundle signed by B was refused afterwards).

## 5. Root key lost or stolen

- **Lost** (every copy or the passphrase): nothing can be added or revoked any more. The current bundle keeps
  working.
- **Stolen**: the thief can sign bundles that trust their own publisher key, and can announce their own
  `next_root`, so §4 cannot be trusted any more. To use it against launchers they still have to serve the bundle
  through your server (or compromise it, §8).

Either way, the server gets a new identity:
1. If stolen: tell players now (§11 "root changed"), with the time of the theft.
2. Ceremony for a new root (§1 steps 1–2): the server now presents the new fingerprint.
3. Rebuild the bundle under the new root with `--previous <latest bundle>`, so versions keep increasing and every
   revocation carries over; if the root was stolen, also revoke every publisher key that could have been added by
   the thief (any key not in your last signed bundle) and reissue your admins' keys (§2). Sign it with the new
   root, then publish with `vgames trust publish --server … --signed … --root <new public key> --new-identity`:
   the server's current bundle was signed by the old root, so only its version is checked.
4. Every launcher blocks the server as a fingerprint mismatch until its player removes the server and adds it
   again. Give them the new fingerprint and a `vgames://server/add` link through a channel they already trust.

## 6. Updater key or code-signing certificate compromised

The updater accepts a build only if it verifies under the public key compiled into the installed launcher, is
signed for exactly the version it claims (`requireSignedVersion`) and is newer than the running one. A stolen
key is dangerous together with the ability to publish a release of this repository, so treat both as suspect.

1. Record T0. Remove `TAURI_SIGNING_PRIVATE_KEY` and its password from the `release` environment, and check
   who could read them (environment reviewers, recent approvals of `release` runs).
2. Check the repository's releases: delete any release, `latest.json` or asset nobody on the team made. Check
   recent workflow runs in the `release` environment and the repository's audit log.
3. Generate a new updater key (release.md, one-time setup step 3) on an offline machine.
4. Ship a release that carries the **new** public key in `apps/desktop/src-tauri/tauri.conf.json`, signed with
   the **old** key (installed launchers accept nothing else). After updating, launchers accept only the new key.
   Store the new key in `release` and keep an offline backup.
5. If the old key is lost rather than stolen, installed launchers cannot update any more: players download the
   new installer and check it (release.md, "Verifying a release independently").
6. Code-signing certificate: ask the CA (Authenticode) or Apple (Developer ID) to revoke it with the compromise
   date, get a new one, store it in `release`, re-sign and re-release. Revocation can also invalidate legitimate
   builds signed after that date: re-release those.
7. Tell players (§11 "launcher update"), including which versions to avoid if a malicious build was published.

## 7. Runtime-catalog key compromised

A stolen catalog key can pin arbitrary runtime archives (Proton, Wine, …), which launchers download and run for
Windows games on Linux and macOS. It still has to be published as this repository's `runtimes` release.

1. Remove `VGAMES_RUNTIME_CATALOG_KEY` and its password from `release`. Delete any catalog asset in the
   `runtimes` release that your team did not publish, and check recent workflow runs.
2. New key on an offline machine (release.md, "Runtime catalog"); commit the new `runtimes/runtime-catalog.pub`
   (security code owners review it) and ship a launcher release carrying it.
3. Publish the next catalog signed with the new key (release.md, "Runtime catalog"), with a version above anything
   the thief published.
4. Launchers keep the highest catalog version they have seen and refuse lower ones (no rollback). If the thief
   published a very high version, legitimate catalogs are refused until launchers track that version per catalog
   key, so a key change starts over (finding for Agent 2, who wires the catalog into the launcher).
5. Tell players on Linux and macOS (§11 "launcher update").

## 8. Compromised server

Someone had control of the API host, database, storage or the server's secrets. By design they could **not**
make launchers run unsigned or tampered code (invariants 1 and 2) or read messages (invariant 3). They **could**
read metadata (accounts, libraries, who talks to whom), act as any account through the API, withhold newer trust
bundles from launchers that have not fetched them yet, and add a device to a user's account (05-social §4).

1. Record T0. Take the API offline (maintenance page) and snapshot disks, database and logs for investigation
   before changing anything.
2. Rotate every server secret: reset `DISCORD_CLIENT_SECRET` in the Discord developer portal, new passwords for
   both database roles, new GCS service-account credentials, new `VGAMES_FS_URL_SIGNING_KEY` (outstanding links
   stop working; launchers get new ones), new `VGAMES_SERVER_SECRET` (only resets page cursors).
3. Rebuild on a clean host from a verified image (`release-api.yml` signs it with cosign keyless and attaches
   provenance; verify both before deploying). If the database's integrity is in doubt, restore the last backup
   from before the compromise.
4. End every session (everyone signs in again):
   ```sql
   UPDATE sessions SET revoked_at = now(), revoked_reason = 'admin' WHERE revoked_at IS NULL;
   ```
   Restarting the API closes open realtime connections.
5. Review, for the compromise window: owner and admin roles, the audit log (`/v1/admin/audit-log`), users
   enabled or allowlisted, devices added (`devices.created_at`; remove unknown ones).
6. Check the trust chain: the server's bundle (`GET /v1/trust/bundle`) must be the last one you signed
   (`vgames trust verify --signed <your last bundle> --root <root public key>`, same version), and every current
   release must pass `vgames verify`.
7. Tell players (§11 "server incident"): sign in again, check where they are signed in, and compare safety
   numbers with the contacts they rely on.

Publisher keys and the root never touch the server, so they need no rotation unless an admin's machine was
compromised too (§3).

## 9. Compromised admin account

Signs: audit-log entries nobody recognizes, unexpected versions, role or allowlist changes.

1. The owner disables the account (admin UI → Users, `PATCH /v1/admin/users/{id}`). This revokes its sessions and
   closes its connections at once; the server then also refuses uploads under the admin's publisher keys, since
   finalize requires the key's holder to be the caller.
2. Read the audit log filtered to that account from the earliest suspect time: packages, versions (uploaded,
   published, yanked), users and allowlist, and for owners settings, roles and trust bundles. Undo what was not
   theirs: yank versions, restore roles, remove allowlist entries.
3. A stolen web session alone cannot sign: publishing needs the key file and its passphrase. If the admin's
   **machine** may be compromised as well, revoke their publisher keys (§3, with `--finalized-before`).
4. The admin secures their Discord account (new password, two-factor authentication, remove unknown authorized
   apps), since vgames sign-in goes through Discord. Only then does the owner enable the account again.
5. If it was the only owner: the Discord id in `VGAMES_BOOTSTRAP_OWNER_DISCORD_ID` can always sign in as owner, so
   secure that Discord account first; if it is lost, set the variable to a new owner's Discord id and restart.

## 10. Antivirus false positives on the overlay injector

On Windows the launcher starts a game suspended and injects the signed overlay DLL (05-social §6). Some antivirus
heuristics flag that technique. If the overlay cannot load, the game still starts without it.

1. Make sure it **is** a false positive. From a report (detection name, file path, launcher version):
   - the file's SHA-256 matches the release's `SHA256SUMS`;
   - its Authenticode signature is valid and ours (`Get-AuthenticodeSignature <file>` in PowerShell);
   - its build provenance verifies: `gh attestation verify <file> --repo wouhliss/vgames`.

   Any mismatch means a real incident: §6.
2. Report the false positive to each vendor that flags it, through their submission portal (Microsoft: the
   Security Intelligence file submission, as a software developer). Include the signed file, the release link,
   the source link and one paragraph on what the injector does and why.
3. Tell affected players (§11 "antivirus"): the overlay is optional and games play without it. Do not ask players
   to turn off their antivirus or add exclusions.
4. Note the vendor, detection name and outcome in the release checklist, so the next release is submitted to
   that vendor before it ships.

## 11. Communication templates

Short, plain, no jargon. Fill the `<…>`. Post where players already look (the server's MOTD in the admin UI,
your announcement channel). Say what happened, what it means for them, what to do, and when you will update.

**Publisher key revoked** (no player action beyond verifying):
> We replaced a signing key used to publish games on <server>. If a game says its signature was replaced, choose
> Verify (or reinstall it). Nothing else changes for you.

**Server incident:**
> On <date> we found that <server> was accessed without permission between <start> and <end>. Games you
> installed could not have been altered: the launcher checks every file against our signatures. Messages are
> end-to-end encrypted and were not readable. Account details and activity on the server may have been seen.
> We have signed everyone out: please sign in again, check "Where you're signed in" in Settings, and compare
> safety numbers with the contacts you rely on. Next update: <time>.

**Root changed** (the launcher will say the server's identity changed):
> <server> has a new identity key. Your launcher will block the server until you add it again: remove it, then
> add it with this link <vgames://server/add…> or check that the launcher shows <fingerprint>. If the launcher
> shows anything else, do not continue and tell us.

**Launcher update:**
> Please update vgames to <version> now (the launcher offers it, or download it from <release link>).
> <Versions to avoid, if any.> <Why, in one sentence.>

**Antivirus:**
> Some antivirus programs flag the vgames in-game overlay by mistake. We have reported it to <vendor>. Games play
> normally without the overlay until they fix it; please do not turn off your antivirus.

**Admin account (to the other admins):**
> <admin>'s account was disabled on <date, time> after <what was seen>. Their changes since <time> are being
> reviewed; do not publish or yank anything for <package list> until we say so. <Their publisher key was / was
> not> revoked.
