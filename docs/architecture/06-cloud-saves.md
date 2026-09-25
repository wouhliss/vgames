# 06 — Cloud Saves

Goal: play on machine A, continue on machine B, never silently lose progress.

## 1. Where saves are

Declared per version in the **signed** manifest (`saves.locations`, 02-package-format §5):

| `base` | Windows | Linux | macOS |
|---|---|---|---|
| `install` | install dir | install dir | install dir |
| `home` | `%USERPROFILE%` | `$HOME` | `$HOME` |
| `documents` | Known Folder Documents | XDG documents dir | `~/Documents` |
| `saved_games` | Known Folder Saved Games | `$XDG_DATA_HOME` | `~/Library/Application Support` |
| `appdata` | `%APPDATA%` | `$XDG_CONFIG_HOME` | `~/Library/Application Support` |
| `localappdata` | `%LOCALAPPDATA%` | `$XDG_DATA_HOME` | `~/Library/Application Support` |
| `xdg_data` / `xdg_config` | (as appdata/localappdata) | XDG dirs | as `appdata` |

Resolved with the OS known-folder APIs (not env vars alone). For Windows builds running through Proton or
Wine, bases resolve **inside the Wine prefix** (09-compatibility §6), and snapshots store Windows-relative
paths, so saves roam between Windows, Linux and macOS. `path` passes the manifest
path rules, and the resolved directory is canonicalized and must stay under its base.
`include`/`exclude` are globs (`globset`), evaluated on `/`-separated relative paths.
Limits: 10,000 files and 512 MiB per package by default (server quota
`VGAMES_SAVE_QUOTA_BYTES_PER_PACKAGE`).

Packages without `saves.locations` have no cloud saves (UI says so, no guessing).

## 2. Data model

- **Blob**: file content, content-addressed per user by BLAKE3; object `v1/{user_id}/{blake3}` in the saves bucket.
- **Snapshot**: immutable list of `{root, path, size, blake3, mtime}` plus `parent_id`, device, platform.
- **Head**: pointer to the latest snapshot per (user, package), moved only by compare-and-swap.

The launcher keeps a local **sync state** per (server, package):
`{base_snapshot_id, files: {root/path → (size, mtime, blake3)}}`. It is the last state known to
equal a snapshot.

## 3. Sync algorithm (launcher `saves` module)

**Before launch (pull):**

1. `GET /v1/saves/{package_id}/head` (timeout 5 s; offline → launch with local saves and mark
   "sync pending").
2. Scan local save files (size+mtime quick check; hash only changed files).
3. Decide:

| Local changed since base? | Head == base? | Action |
|---|---|---|
| no | yes | Nothing |
| no | no | **Restore** head: download missing blobs → write each file to a temp name in the same dir → fsync → rename. Files not in head are moved to backup. Update base. |
| yes | yes | Nothing now (local progress is newer; pushed after the session) |
| yes | no | **Conflict** → dialog (below) |

**After exit (push)**, when the whole process tree has exited (and 3 s grace for flushes):

1. Scan. No changes vs base → done.
2. `POST /v1/saves/{package_id}/blobs/prepare {blobs:[{blake3,size}]}` → missing blobs + signed PUT URLs →
   upload in parallel (verify the returned object size).
3. `POST /v1/saves/{package_id}/snapshots {parent_snapshot_id: base, files:[…], device_id, platform}`
   → `201` (new head) or `409 save_head_conflict` (another device pushed meanwhile) → conflict dialog.
4. Update base to the new snapshot.

**Conflict dialog** (never auto-resolves):
"Your saves on this device and in the cloud both changed." Shows both timestamps, device names and
file counts. Options: **Keep cloud** (local moved to backup, restore head), **Keep this device**
(push local with `parent = head`), **Keep both** (push local as a new head, keep the old head
reachable in history). Cancel = do nothing and do not launch.

**Backups:** before any overwrite or delete, the affected local files are copied to
`<app data>/save-backups/<package>/<timestamp>/` (the 5 most recent kept). Settings → Cloud saves → History lists
server snapshots and local backups with **Restore**.

## 4. Server rules (Agent 1)

- `blobs/prepare`: validate hashes/sizes, enforce quota, insert `save_blobs` rows with `uploaded_at = NULL`,
  return URLs (PUT, 15 min, `x-goog-content-length-range: size,size`). Blobs already uploaded are not returned.
  Unknown (or deleted) package → `404`.
- **Quota** (`VGAMES_SAVE_QUOTA_BYTES_PER_PACKAGE`, distinct blob bytes): `prepare` answers `413 save_quota_exceeded`
  when the head's blobs + the pushed blobs + the user's uploads still in flight (prepared in the last hour, not yet
  committed) exceed it; a commit whose own distinct blobs exceed it is `413` too. After a commit, the oldest snapshots
  of that package (never the head) are dropped until the retained snapshots fit the quota, so history shrinks instead
  of blocking new saves.
- Snapshot commit (one transaction): every referenced blob exists **and** is uploaded (the server
  confirms via object metadata, including size, the first time it is referenced) → insert snapshot and
  `save_snapshot_blobs` → `UPDATE save_heads SET snapshot_id=$new WHERE user_id=$u AND
  package_id=$p AND snapshot_id=$parent` (or insert if the parent is null and no head exists). Zero rows → rollback, `409`.
- Retention job: keep the latest 20 snapshots + the head; delete blobs no longer referenced (and their objects)
  once a day has passed since their last `prepare` or upload (a push in progress keeps its blobs). Deletion holds a
  per-user lock that `prepare` and commits share, so a blob confirmed for a commit cannot disappear before the
  snapshot references it.
- A user can only ever see their own saves. There is no admin UI for save contents.
