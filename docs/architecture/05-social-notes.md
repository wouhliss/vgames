# 05 — Social: adaptation notes and launcher interface

Status: **v1** (A4-T01, 2026-09-25). Companion to [05-social.md](05-social.md). It records how the
Arachnel mechanisms map onto vgames, the few protocol details 05-social left open, and the exact
realtime events, Tauri commands and Tauri events Agent 4 delivers. **Agent 3 can build every social
screen against mockIPC from §5 and §6 alone.** Changing a name or shape here needs a `contract:` PR.

## 1. Arachnel → vgames

Arachnel (`BadKiko/Arachnel`, Qt/C++) was read for ideas only; no code was copied.

| Arachnel mechanism | How it works there | vgames | Why it changed |
|---|---|---|---|
| **Friend codes** (`InviteService::createInviteCode` / `acceptInviteCode`, `FriendCodePin.qml`) | 6 digits from an anonymous relay; accepting immediately makes both sides friends; identity = self-chosen display name + device public key | `POST /v1/friend-codes` → 8 chars of Crockford base32 (`^[0-9A-HJKMNP-TV-Z]{8}$`), single use, 15 min, max 30 redemptions per user per hour. Redeeming (`POST /v1/friends/requests {friend_code}`) creates a **request** the code owner must accept (a leaked code cannot force a friendship). Identity is the server's Discord-backed user. | 10⁶ codes are enumerable at the old rate limits; 32⁸ ≈ 1.1 × 10¹² are not. Self-asserted names let anyone impersonate anyone. |
| **Code entry UI** (`FriendCodePin.qml`: fixed cells, digits only, auto-submit when complete) | | Same pattern, 8 cells. The UI upper-cases input, maps `O→0`, `I/L→1`, drops `U`, `-` and spaces (Crockford decoding rules), and submits when 8 valid characters are present. Rust validates again. | Crockford is forgiving to type and read aloud. |
| **Presence** (`PresenceService::publish` + `refresh`, `SocialController` 5 s `QTimer`) | Polls the relay every 5 s: publishes own state, downloads everyone's | Push. The launcher sends `presence.set` over the realtime socket **on change only**; the server fans `presence.changed` out to accepted friends only. The idle launcher sends nothing but WebSocket pings/pongs. | Polling breaks the ~0% idle CPU budget and leaks presence to anyone who asks the relay. |
| **Current game on presence** (`currentGameId/Title/CoverUrl` from the client) | Client sends title and cover URL; relay trusts them | Client sends only `package_id`; the server resolves the title from its own catalog (`package_title`) and covers come from `/v1/assets/…`. "Show what I'm playing" off → `in_game` without `package_id`. | A client-supplied cover URL is an SSRF/tracking pixel vector and a spoofing vector. |
| **Suggestion card** (`suggestGame`, `SuggestionOverlayCard.qml`: poster, "Suggested by X", Open, dismiss) | One-way "look at this game"; the receiver sees a non-modal card and can open the game page | Becomes the **invite handshake** (05-social §5): `invite-received` event → the UI shows the same non-modal, dismissible card (poster, "X invites you to play", **Accept** / **Decline**, close = decide later). Accept runs the install-if-needed flow and reports progress back to the sender. In a game, the overlay shows the card as a toast. | A suggestion has no state; an invite needs accept/decline, install progress, readiness and a join secret. |
| **Running-game bar** (`RunningGameBar.qml`) | Shows the running game | Agent 3's running-game strip, driven by Agent 2's `game-started`/`game-stopped`. Agent 4 subscribes to the same events for presence (`in_game`) and the overlay lifecycle (broker per game). | One source of truth for "a game is running". |
| **Relay URL setting** | Any relay, typed by the user | No relay: social runs on the signed-in vgames server; one socket per active server. | The server already authenticates users. |
| **Remove friend** (`removeFriend` tolerates 404) | | `DELETE /v1/friends/{user_id}` (friend or outgoing request); the launcher treats 404 as "already gone". | Same idea. |

## 2. Protocol details fixed here

### 2.1 Device and one-time-key signatures (canonical JSON)

The server and every launcher build these strings with **one** implementation:
`vgames_proto::social::canonical` (A4-T02). Canonical JSON = the object below with keys sorted by
byte value, no whitespace, strings in JSON minimal escaping (all values here are base64, UUIDs or
booleans, so nothing is ever escaped), UTF-8.

Device keys (signed by the device's Ed25519 `signing_key` at registration):

```json
{"identity_key":"<curve25519 b64>","server_id":"<uuid>","signing_key":"<ed25519 b64>","type":"vgames.device_keys/1","user_id":"<uuid>"}
```

One-time key and fallback key (signed by the same `signing_key`):

```json
{"fallback":false,"key":"<curve25519 b64>","key_id":"<vodozemac key id>","type":"vgames.otk/1"}
```

- Keys and signatures are **unpadded standard base64** as vodozemac produces them (43 and 86 chars).
- The server verifies both with Ed25519 strict verification before storing (`400 bad_signature`).
- Launchers verify the device signature when they first see a device (`GET /v1/users/{id}/devices`),
  check `user_id`/`server_id` equal what they asked for, and verify every claimed key's signature
  against the pinned `signing_key` before creating an outbound session. `fallback` is signed so a
  server cannot relabel a one-time key as a reusable fallback key.

### 2.2 Safety number (05-social §4.2)

For each user: the **sorted** list of their known, non-revoked devices' `identity_key ‖ signing_key`
(raw 32 + 32 bytes, sorted bytewise). With the two users ordered by user id (bytes of the UUID):

```
fp(u)  = BLAKE3.derive_key("vgames 2026-09 safety number user v1", uuid(u) ‖ u32be(n) ‖ keys₁ ‖ … ‖ keysₙ)
out    = BLAKE3.derive_key("vgames 2026-09 safety number v1", fp(low) ‖ fp(high)), extended to 60 bytes (XOF)
digits = for each of 12 chunks of 5 bytes: big-endian 40-bit integer mod 100000, zero-padded to 5 digits
```

60 digits in 12 groups of 5. Both sides compute the same number; any device change on either side
changes it. Signing keys are included on top of 05-social's "identity keys" so a swapped signing key
also shows.

### 2.3 Plaintext payload (inside Olm only)

As 05-social §4.1, JSON, ≤ 60 KiB before encryption. `type` values the launcher sends and accepts:

| type | fields besides `v`, `type`, `conversation_id`, `client_message_id`, `sent_at` |
|---|---|
| `text` | `body` (1–4,000 chars after trimming trailing whitespace) |
| `invite.join` | `invite_id`, `join_secret` (optional; `^[A-Za-z0-9._:\-\[\]]{1,256}$`) |
| `receipt.read` | `up_to` (a `client_message_id`) |

Anything else, or a payload whose `conversation_id` differs from the envelope's, is stored as
"unsupported message" and never interpreted. `invite.join` is only honoured when it comes from a device
of the invite's sender for an invite this launcher accepted and that is `ready`.

### 2.4 Presence liveness (server)

`user_presence` stays last-writer-wins per user (the schema's PK). Liveness comes from the socket: the
API instance holding a user's socket refreshes a heartbeat every 10 s; a row whose heartbeat is older
than 30 s becomes `offline` (guarded update, so exactly one instance publishes `presence.changed`).
The launcher sends `presence.set` right after every `hello`. `PUT /v1/presence` without a socket
therefore lasts 30 s. `users.last_seen_at` is updated at most once per minute from the same heartbeat.

### 2.5 Invite progress throttling

The server stores every `installing` report but publishes `invite.updated` for progress at most every
2 s per invite (state changes always publish). The launcher reports every ≥ 5 s or ≥ 5 %.

## 3. Server additions (Agent 4)

- Migration `…_social_invites_presence.sql` (new file, social tables only): `game_invites.failure_reason`,
  `game_invites.progress_published_at`, `user_presence.heartbeat_at`.
- Job `social.sweep` (every 60 s): expires invites and friend codes, deletes expired envelopes and
  claimed one-time keys older than 30 days, marks stale presence `offline`.
- Realtime additions (additive to 03-api §6): `presence.changed` may carry `package_title`;
  server → client `typing {conversation_id, user_id}` (the fan-out of the client's `typing`, to the other
  members' sockets only, at most one per user per conversation every 3 s).

## 4. Realtime events (server → launcher)

Envelope as 03-api §6. The launcher treats every event as a nudge; REST stays the source of truth.

| type | data | Launcher reaction |
|---|---|---|
| `hello` | `{user_id, device_id?, server_time}` | Resync friends, invites, conversations and inbox via REST; send `presence.set` |
| `presence.changed` | `{user_id, status, package_id?, package_title?}` | Update the friend, emit `presence-changed` |
| `friend.request` | `{user_id}` | Refetch `/v1/friends`; emit `friends-changed` and `friend-request-received` |
| `friend.accepted` / `friend.removed` | `{user_id}` | Refetch `/v1/friends`; emit `friends-changed` |
| `inbox.new` | `{conversation_id, count}` | `GET /v1/inbox` → decrypt → store → ack; emit `message-received` per message |
| `invite.created` | `{invite}` | Emit `invite-received` (incoming) or `invite-changed` (outgoing); overlay toast |
| `invite.updated` | `{invite}` | Emit `invite-changed`; sender: when `ready`, send `invite.join` over Olm |
| `device.added` | `{user_id, device_id}` | Fetch the user's devices, pin the new one (TOFU), emit `device-notice` |
| `device.revoked` | `{user_id, device_id}` | Mark revoked, drop its sessions, emit `device-notice` |
| `typing` | `{conversation_id, user_id}` | Emit `typing` |
| `session.revoked` | `{reason}` | Stop the socket; Agent 2's auth module signs out |

Client → server: `presence.set {status, package_id?}`, `typing {conversation_id}`, `ping`.

## 5. Tauri commands (main window)

Conventions as in `apps/desktop/src/ipc/contract.ts`: snake_case command names and payload fields,
camelCase argument keys, `Result` mode, enums `#[serde(tag = "kind", rename_all = "snake_case")]`.
Every command applies to the **active server**. Timestamps are RFC 3339 strings; ids are UUID strings.

```ts
// ---- errors -------------------------------------------------------------------------------
export type SocialError =
  | { kind: "not_signed_in" }                         // no active server or no session
  | { kind: "offline" }                               // the server could not be reached
  | { kind: "not_found" }                             // unknown, not visible or blocked (indistinguishable on purpose)
  | { kind: "invalid_input"; field: string; message: string }
  | { kind: "rate_limited"; retry_after_seconds: number }
  | { kind: "code_invalid" }                          // friend code unknown, expired or used
  | { kind: "limit_reached"; limit: "friends" | "pending_requests" | "party_members" }
  | { kind: "conflict"; code: string; message: string } // e.g. invalid_state_transition (someone else acted first)
  | { kind: "key_changed"; user_id: string; device_ids: string[] } // sending blocked until re-trusted (§5, contact_trust_device)
  | { kind: "server"; code: string; message: string }
  | { kind: "internal"; detail: string };

// ---- people -------------------------------------------------------------------------------
export type UserSummary = { id: string; username: string; display_name: string | null; avatar_url: string | null };
export type PresenceStatus = "online" | "away" | "in_game" | "offline";
export type Presence = { status: PresenceStatus; package_id: string | null; package_title: string | null; updated_at: string | null };
export type Friend = { user: UserSummary; state: "accepted" | "incoming" | "outgoing"; presence: Presence | null; since: string | null };
export type FriendList = { friends: Friend[]; incoming: Friend[]; outgoing: Friend[] };
export type FriendCode = { code: string; expires_at: string };
export type FriendTarget = { kind: "code"; code: string } | { kind: "user"; user_id: string };
export type BlockedUser = { user_id: string; username: string | null; blocked_at: string };
export type SocialConnectionState = "signed_out" | "connecting" | "connected" | "reconnecting";
export type SocialConnection = { server_id: string | null; state: SocialConnectionState; retry_at: string | null };
export type SocialSettings = {
  show_current_game: boolean;   // default true (05-social §3)
  do_not_disturb: boolean;      // suppresses toasts except invites from favorites
  overlay_enabled: boolean;     // global default; per-package toggles live on the package page
  overlay_hotkey: string;       // accelerator, default "Shift+F3"
};

// ---- messaging ----------------------------------------------------------------------------
export type Conversation = {
  id: string; kind: "direct" | "party"; members: UserSummary[];
  last_message: Message | null; unread: number; created_at: string;
};
export type MessageStatus = "pending" | "sent" | "failed" | "received";
export type DeviceNotice =
  | { kind: "new_device"; user_id: string; device_id: string; device_name: string }
  | { kind: "key_changed"; user_id: string; device_id: string }
  | { kind: "device_revoked"; user_id: string; device_id: string };
export type MessageBody =
  | { kind: "text"; text: string }
  | { kind: "invite_join"; invite_id: string }
  | { kind: "notice"; notice: DeviceNotice }
  | { kind: "unsupported" };
export type Message = {
  id: string; conversation_id: string; sender_user_id: string; mine: boolean;
  body: MessageBody; sent_at: string; received_at: string | null; status: MessageStatus;
};
export type ContactDevice = {
  device_id: string; display_name: string | null; first_seen_at: string;
  key_fingerprint: string;                            // identity key, grouped for display
  state: "trusted" | "new" | "key_changed" | "revoked";
};
export type ContactSecurity = {
  user_id: string; verified: boolean; needs_reverification: boolean;
  safety_number: string;                              // 60 digits
  safety_number_groups: string[];                     // 12 × 5 digits
  devices: ContactDevice[];
};
export type MyDevice = {
  id: string; display_name: string; platform: "windows" | "linux" | "macos";
  current: boolean; created_at: string; last_seen_at: string | null;
};

// ---- invites ------------------------------------------------------------------------------
export type InviteState = "pending" | "accepted" | "installing" | "ready" | "joined" | "declined" | "cancelled" | "expired" | "failed";
export type InviteFailure = "no_build_for_platform" | "install_failed" | "insufficient_space" | "cancelled_by_user";
export type InvitePackage = { id: string; title: string; cover_url: string | null }; // cover_url is a vgimg: URL
export type Invite = {
  id: string; direction: "incoming" | "outgoing"; from: UserSummary; to: UserSummary; package: InvitePackage;
  state: InviteState; progress: number | null; message: string | null; failure_reason: InviteFailure | null;
  created_at: string; updated_at: string; expires_at: string;
  has_join_secret: boolean;                           // outgoing only (the secret itself never leaves Rust)
};
```

| Command | Arguments | Returns |
|---|---|---|
| `social_connection` | — | `SocialConnection` (infallible) |
| `social_settings_get` / `social_settings_set` | — / `settings: SocialSettings` | `SocialSettings` |
| `friends_list` | — | `FriendList` |
| `friend_code_create` | — | `FriendCode` |
| `friend_request_send` | `target: FriendTarget` | `Friend` (`accepted` if they had already asked you) |
| `friend_accept` | `userId` | `Friend` |
| `friend_decline` / `friend_remove` | `userId` | `null` |
| `user_block` / `user_unblock` | `userId` | `null` |
| `blocks_list` | — | `BlockedUser[]` (kept locally; the server has no list endpoint) |
| `user_profile` | `userId` | `UserSummary` |
| `conversations_list` | — | `Conversation[]` (most recent first) |
| `conversation_open_direct` | `userId` | `Conversation` |
| `conversation_create_party` | `userIds: string[]` (1–15 friends) | `Conversation` |
| `messages_list` | `conversationId`, `before: string \| null` (message id), `limit: number` (1–200) | `Message[]` (oldest first) |
| `message_send` | `conversationId`, `text` | `Message` (`pending`, then `message-status-changed`) |
| `message_retry` | `messageId` | `Message` |
| `conversation_mark_read` | `conversationId` | `null` |
| `typing_start` | `conversationId` | `null` (throttled in Rust) |
| `contact_security` | `userId` | `ContactSecurity` |
| `contact_set_verified` | `userId`, `verified: boolean` | `ContactSecurity` |
| `contact_trust_device` | `userId`, `deviceId` | `ContactSecurity` (accepts a changed key and unblocks sending) |
| `devices_list` | — | `MyDevice[]` |
| `device_revoke` | `deviceId` | `null` |
| `invites_list` | — | `Invite[]` |
| `invite_send` | `toUserId`, `packageId`, `message: string \| null`, `joinSecret: string \| null` | `Invite` |
| `invite_accept` / `invite_decline` / `invite_cancel` | `inviteId` | `Invite` |

## 6. Tauri events

| Event name | Payload | When |
|---|---|---|
| `social-connection-changed` | `SocialConnection` | Socket state changes |
| `friends-changed` | `FriendList` | After any friend-list resync |
| `presence-changed` | `{ user_id: string; presence: Presence }` | A friend's presence changed |
| `friend-request-received` | `{ user: UserSummary }` | Someone sent you a request (toast) |
| `invite-received` | `Invite` | A new incoming invite: show the suggestion-style card |
| `invite-changed` | `Invite` | Any invite update (both directions) |
| `invite-install-requested` | `{ invite_id: string; package: PackageRef; reason: "missing" \| "outdated" }` | Open the install/update dialog **at once** (05-social §5); install through Agent 2's normal commands |
| `conversations-changed` | `Conversation[]` | Membership, ordering or unread counts changed |
| `message-received` | `Message` | A decrypted incoming message (or a device notice) was stored |
| `message-status-changed` | `{ message_id: string; conversation_id: string; status: MessageStatus }` | Outgoing message sent / failed |
| `typing` | `{ conversation_id: string; user_id: string }` | Show "… is typing" for 5 s |
| `device-notice` | `{ conversation_id: string \| null; notice: DeviceNotice }` | New device, key change, revocation |

`PackageRef` is Agent 2's `{ server_id, package_id }`.

## 7. Overlay window (macOS panel and fallback window)

The overlay window (label `overlay`) receives only `overlay-view` events and may call only:

| Command | Arguments | Returns |
|---|---|---|
| `overlay_view` | — | `OverlayView` |
| `overlay_action` | `action: OverlayAction` | `null` |

```ts
export type OverlayToast = { id: string; kind: "invite" | "message" | "friend_online"; title: string; body: string; expires_at: string };
export type OverlayView = {
  visible_panel: boolean;
  toasts: OverlayToast[];
  friends_online: { user_id: string; name: string; status: PresenceStatus; playing: string | null }[];
  invites: { invite_id: string; from: string; package_title: string; state: InviteState }[];
  recent_messages: { conversation_id: string; from: string; text: string; sent_at: string }[];
};
export type OverlayAction =
  | { kind: "accept_invite"; invite_id: string }
  | { kind: "decline_invite"; invite_id: string }
  | { kind: "quick_reply"; conversation_id: string; text: string }
  | { kind: "open_launcher" }
  | { kind: "close_panel" };
```

The in-game renderer (injected DLL / Vulkan layer / GL preload) gets the same view model over the
loopback broker (`vgames_overlay::protocol`), never through the WebView.
