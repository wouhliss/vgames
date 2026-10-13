# Cloud-save launcher interfaces (PLAY-06)

This additive proposal implements [06-cloud-saves](06-cloud-saves.md) without changing the server's HTTP
contract. Commands and events use generated Tauri types. Every package key is a `PackageRef`
(`server_id`, `package_id`); every server request uses that server's authenticated session. No command accepts
an absolute path, signed URL, user id, device access token, or an arbitrary server origin from the WebView.

## Commands and summaries

All commands return the launcher's `Result<T, AppError>` envelope. UUIDs are strings in TypeScript.
Timestamps are UTC RFC 3339 strings. Unavailable timestamps remain null; the UI does not invent dates.

| Native command | Parameters | Result |
|---|---|---|
| `saves_status` | `pkg: PackageRef` | `SaveStatus` |
| `saves_history` | `pkg: PackageRef` | `SaveHistory` |
| `saves_restore` | `pkg: PackageRef, source: SaveSource` | `SaveStatus` |
| `save_conflict_get` | `conflict_id: Uuid` | `SaveConflict` |
| `save_conflict_resolve` | `conflict_id: Uuid, choice: ConflictChoice` | `SaveStatus` |

`SaveStatus` has `package`, `state: CloudSaveState`, `last_synced_at: string | null`, and
`conflict_id: Uuid | null`. The existing states stay `unsupported | synced | syncing | pending | conflict`.
`unsupported` means the verified manifest declares no save locations; `pending` means work remains after an
offline request or retryable failure. `conflict` always has a durable conflict id. No error silently selects
either side of a conflict.

`SaveSource` is a tagged union `{kind: "cloud", snapshot_id: Uuid}` or
`{kind: "backup", backup_id: Uuid}`. The Rust service resolves it to a record belonging to the supplied
package and active local account. A backup id is an opaque database identifier, never a filesystem path.
Unknown or wrong-package sources fail without touching save files.

`SaveSummary` has `source`, `created_at`, `device_name`, `file_count`, and `total_bytes`.
`SaveHistory` has `package`, `entries: SaveSummary[]`, and `head_snapshot_id: Uuid | null`.
The client follows the server's cursor pagination with a bounded request deadline and cancellation;
local history includes the five retained backup records. Display names are plain text.

`SaveConflict` has `conflict_id`, `package`, `cloud: SaveSummary`, `device: SaveSummary`, and `created_at`.
The device summary's source identifies the protected local backup made for this conflict; its files may
have changed since detection, so resolution rescans before making a new snapshot or restoring anything.
Choices are exactly `keep_cloud | keep_device | keep_both | cancel`:

- `keep_cloud`: protect the current local files in a backup, restore the selected cloud head, then update the base.
- `keep_device`: rescan the current local files and push with the current cloud head as the parent.
- `keep_both`: protect the old cloud head in a local backup, then push the current local files with that head
  as parent. Both remain reachable through history subject to the documented server retention and five-backup limit.
- `cancel`: make no filesystem or server changes. Keep the unresolved conflict and do not launch automatically.

A changed cloud head during resolution causes a renewed conflict, never an unconditional overwrite.
Successful resolution does not launch a game; the player explicitly retries launch. Restore or resolution
is refused while that package's process tree is running, and serializes with launch preparation and sync.

## Events and launch veto

`save-sync-changed` carries `SaveStatus`. The UI refreshes only that package's tile and settings row.
`save-conflict` carries `SaveConflict`. The dialog opens from this event or by looking up the conflict id
in a failed-launch toast. The existing `LaunchError::SaveConflict { conflict_id }` remains the veto.
Consumers may miss a broadcast event; both commands reread durable state after restart or reconnect.

The conflict dialog shows the time, device name and file count for each side and all four choices.
It never automatically resolves or retries launch. Offline preparation permits launch with a visible pending
state; conflicting changes veto launch. Missing saved credentials do not cause requests to another account.

## Ownership, paths and durability

A launch UUID identifies each hook invocation. `before_spawn` reads only the save configuration from the
verified signed manifest. Native locations use OS known folders; Proton/Wine use `compat::save_base` and
`compat::prefix_dir`, with Windows-relative snapshot names on every host. `install` remains the verified
install root. Save traversal rejects escapes, symlinks/reparse points, and collisions between resolved locations.
The existing 10,000-file and 512 MiB limits apply before uploads or restores.

Security invariant 1 takes precedence over the `install` base: server snapshots are not publisher-authenticated package bytes. Until an explicit security contract defines safe install-local saves, restore refuses every write or deletion inside any installed directory, including install-based locations. Native installed-directory inventory also protects installs reached from another known-folder base. Before staging, validate the entire replacement/deletion set against the verified manifest location ids, include/exclude filters (including excluded ancestor directories), portable path/tree rules and size/count limits. This preflight does not replace no-follow handles, launch exclusion, verified blobs, durable backups or recovery journals.

A five-second pull budget and cancellation apply to head lookup and staging; canceled background work cannot
later overwrite files used by a running game. File replacement uses a same-directory temporary file, fsync,
and atomic rename, with a durable recovery record and a protected backup before any overwrite or removal.
On restart recovery completes before save files are used. The base is updated only after the restore finishes;
killing restore must leave each file complete and retain sufficient records to recover the interrupted operation.

`after_exit` starts after the whole process tree exits, waits a cancellable three-second flush grace, then scans
size and mtime and hashes changed files. Snapshot commits use a stable persisted `Idempotency-Key` for retries
of the same operation. A new payload receives a new key. `blob_not_uploaded` triggers preparation/upload recovery;
`save_head_conflict` creates a conflict; quota errors leave local files intact and sync pending. URLs remain in
Rust and logs are redacted. Backups are scoped by server, package and account to prevent cross-server collisions.

Append-only SQLite migrations extend `save_sync_state` and add durable conflict, backup and recovery records.
Do not edit a merged migration. The `installs` cloud-save provider reads this service's cached state, with no
network access inside `installs_list`. Pending IPC entries and mocks are a separate ordinary PR; native command
implementation replaces those entries with generated bindings.

## Required evidence

The required Linux desktop job runs the real API with two simulated devices, covering all four decision-table
rows, all four choices, offline pull, quota, missing blob recovery and competing-head commits. Process-kill
restore tests verify complete files and recovery. A hosted Windows → Linux → macOS save round trip transfers
real server state between jobs and checks the same Windows-relative snapshot paths. UI tests exercise all choices,
pending notices, history and confirmed restore, including restart, accessibility, keyboard and gamepad navigation.
Physical-device save checks remain in the human checklist B5; automated evidence links belong in `play.md`.
