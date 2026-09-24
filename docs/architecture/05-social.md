# 05 — Social: Friends, Presence, E2EE Messaging, Invites, Overlay

## 1. Lineage: what we take from Arachnel

[`BadKiko/Arachnel`](https://github.com/BadKiko/Arachnel) (Qt/C++) has a small,
effective social layer in `src/core/social/`:

| Arachnel | What it does | vgames keeps | vgames changes |
|---|---|---|---|
| `InviteService` + `FriendCodePin.qml` | Short-lived friend codes (6 digits) through a relay (`/v1/social/invites/create`, `/accept`) | Short-lived codes as the easiest way to add a friend | 8-char Crockford base32 (≈ 10¹² space, not 10⁶), single use, 15 min, rate-limited; Discord identity instead of anonymous device keys |
| `PresenceService` | Publishes `online` + current game; **polls every 5 s** | "Playing X" presence on the friend list | Push over WebSocket. No polling (idle CPU budget ≈ 0%) |
| `suggestGame` + `SuggestionOverlayCard.qml` | A friend "suggests" a game; the receiver sees a card over the main window with **Open** | The **in-app overlay card** pattern: non-modal, dismissible, one click to the package | Becomes a real **invite handshake** (accept/decline, auto-install, progress, join) |
| `RunningGameBar.qml` | Shows the running game | Running-game strip in the launcher | Also drives presence and overlay lifecycle |
| Relay trusts client-sent display names/covers | — | — | Server-side identity. Covers come from the server's own assets, never from clients |

No code is copied; vgames reimplements the ideas in Rust/React.

## 2. Friends

- Add a friend by **friend code** (`POST /v1/friend-codes` → share → other side
  `POST /v1/friends/requests {friend_code}`), or by `user_id` from a shared conversation or invite.
  There is no global user search: it keeps the server from becoming a directory and cuts spam.
- A request is `pending` until the addressee accepts. Either side can remove a friend. Blocking removes
  the friendship, hides presence both ways, rejects requests, invites and messages from the
  blocked user (`403 blocked`, same response as "not found" to avoid confirming existence).
- Friend limit 500 per user; pending outgoing requests limit 100.

## 3. Presence

- States: `online`, `away` (no input for 10 min, from the OS idle time), `in_game` (+ `package_id`), `offline`.
- Set by the launcher over realtime (`presence.set`) on changes only; the server stores
  it in `user_presence` (UNLOGGED) and fans out `presence.changed` **to accepted friends only**.
- A socket dropping for > 30 s → `offline`. `users.last_seen_at` updated at most once per minute.
- Privacy setting "Show what I'm playing" (default on): when off, `in_game` is sent without `package_id`.

## 4. End-to-end encrypted messaging

### 4.1 Protocol

- **Olm** (Double Ratchet with 3DH handshake, Curve25519/Ed25519) via **vodozemac 0.11**,
  the Matrix ecosystem's audited Rust implementation. The crypto runs **only in the launcher's Rust
  core**, never in the WebView.
- Each launcher install is a **device** with a vodozemac `Account` (identity Curve25519 key,
  signing Ed25519 key). On first sign-in: `POST /v1/devices` with the public keys and a
  self-signature; then upload 50 signed one-time keys + 1 fallback key.
  Top up when the server reports fewer than 20 left (`GET /v1/devices` includes counts).
- **Sending** (client-side fan-out, used for both DMs and party chats ≤ 16 members):
  1. Target devices = all non-revoked devices of every member, plus the sender's own other devices.
  2. For each target without an Olm session: `POST /v1/keys/claim` (atomic `FOR UPDATE SKIP LOCKED`
     claim of one OTK; fallback key if exhausted), verify the key signature against
     the device's signing key, create an outbound session.
  3. Encrypt the plaintext payload separately for each device and send in one request:
     `POST /v1/conversations/{id}/messages {client_message_id, envelopes:[{recipient_device_id, olm_message_type, ciphertext}]}`.
  4. The server validates membership, device ownership and sizes, inserts one row per envelope
     (idempotent on `(sender_device_id, client_message_id, recipient_device_id)`), and emits `inbox.new`.
- **Receiving:** `GET /v1/inbox` (cursor), decrypt (pre-key messages create inbound sessions),
  persist locally, `POST /v1/inbox/ack` → the server deletes the envelopes.
- **Plaintext payload** (JSON, inside the ciphertext only):

```json
{ "v": 1, "type": "text", "conversation_id": "…", "client_message_id": "…",
  "sent_at": "2026-09-24T10:00:00Z", "body": "gg" }
```

  Types: `text` (≤ 4,000 chars), `invite.join` (§5), `receipt.read`. Unknown types are
  shown as "unsupported message" and never interpreted.

### 4.2 Trust and verification

- The launcher pins each contact device's identity key on first sight (TOFU per device).
  New devices of a contact produce a visible notice in the conversation ("Sam signed in on a new device").
  A **changed** key for a known device id blocks sending to it and warns.
- **Safety number** per contact: 60 digits derived from both users' sorted device
  identity keys (BLAKE3, formatted in 12 groups of 5), comparable out of band.
  Marking a contact "verified" makes any later device change require re-verification.
- Revoking a device (`DELETE /v1/devices/{id}`) revokes its sessions, and contacts receive `device.revoked`.

### 4.3 Local storage

- Olm account and session pickles are encrypted with a 32-byte pickle key from the OS keychain.
- Messages are stored in SQLite with bodies encrypted using XChaCha20-Poly1305 under a
  per-install key in the keychain. Search is local and runs over decrypted rows in memory for the open conversation only.
- Message history does not transfer to new devices (stated in Help). Key backup is a later feature.

### 4.4 Limits

Envelope ciphertext ≤ 64 KiB; ≤ 64 envelopes per send; 120 sends/min per device; undelivered
envelopes expire after 30 days.

## 5. Game invites (handshake)

```
 A's launcher                    API                              B's launcher
   │ POST /v1/invites {to:B, package_id, message?}
   │──────────────────────────────▶│ checks: friends, not blocked, package published
   │                               │ and visible, rate limit; state=pending, expires 10 min
   │                               │── invite.created ──────────────▶│ invite card (in-app, or in-game overlay if a game runs)
   │                               │                                 │ [Accept] [Decline]
   │                               │◀── POST /accept ────────────────│
   │◀─ invite.updated (accepted) ──│                                 │ local check for package_id:
   │                               │                                 │  ├ installed & current → ready
   │                               │                                 │  ├ missing  → install dialog opens at once
   │                               │                                 │  │  (library picker, size, free space) → download
   │                               │◀── POST /status {installing, p} ─│  │  progress reported every ≥ 5 s or 5%
   │◀─ invite.updated (42%) ───────│                                 │  └ outdated → update dialog, same reporting
   │                               │◀── POST /status {ready} ─────────│
   │◀─ invite.updated (ready) ─────│                                 │
   │ E2EE message {type:"invite.join", invite_id, join_secret?} ────────────────────────────────────▶│
   │                               │                                 │ launch join target (manifest `multiplayer.join`)
   │                               │◀── POST /status {joined} ────────│ presence in_game (same package)
```

Rules:

- One active invite per (from, to, package): a second `POST` returns the existing one.
- Expiry: `pending` 10 min; `accepted`/`installing`/`ready` 24 h. The sweeper marks `expired` and emits `invite.updated`.
- The **server never sees join secrets**. A's launcher sends them over Olm. The secret is either
  typed by A ("Server address / lobby code", optional) or empty, meaning "start the game and join in-game".
  B's launcher validates it against `^[A-Za-z0-9._:\-\[\]]{1,256}$` and substitutes it only as a whole
  argument into the manifest's `multiplayer.join.args`. Invalid or absent → normal launch.
- If B has no access to a platform build of the package → `failed` with reason `no_build_for_platform`.
- Install prompted by an invite uses the normal install path (signature checks etc.). Nothing is
  skipped because a friend asked.

## 6. Overlay

Packages distributed through vgames carry **no anti-cheat**, so the overlay renders **inside the
game process**, as the Steam and Discord overlays do. That makes it work in exclusive fullscreen too.
It stays lightweight: a Rust library drawing a small Dear ImGui interface, loaded only into games the
launcher starts, only while they run.

### 6.1 Architecture

```
 Launcher (Rust core)                                   Game process
 ┌──────────────────────────────┐   loopback TCP     ┌───────────────────────────────────────────┐
 │ overlay broker (per game)    │◀──────────────────▶│ vgames-overlay (injected DLL / Vulkan      │
 │ • view models: toasts,       │ 127.0.0.1:<random>  │ layer / GL preload)                       │
 │   friends online, invites,   │ + 32-byte session   │ • hooks Present/SwapBuffers/QueuePresent  │
 │   last messages (plaintext)  │   token (env var)   │ • Dear ImGui panel + toasts               │
 │ • actions: accept/decline,   │ length-prefixed     │ • input capture while the panel is open   │
 │   quick reply, open launcher │ postcard frames     │ • no network, no keys, no disk writes     │
 └──────────────────────────────┘                     └───────────────────────────────────────────┘
```

- All social logic, crypto and network stay in the launcher. The in-game module is a thin renderer
  that shows view models and sends back user actions. Decrypted message text crosses into the game
  process only for display; the game runs as the same OS user and could read it anyway.
- Loopback TCP (not named pipes or Unix sockets) works identically on Windows, inside Proton/Wine,
  and inside the pressure-vessel container. The broker listens only while a game runs, accepts
  exactly one connection carrying the session token (`VGAMES_OVERLAY_TOKEN`, 32 random bytes), and
  closes the listener after the handshake.
- Frame cost budget: ≤ 0.3 ms CPU and ≤ 0.3 ms GPU per frame while hidden, ≤ 1 ms while the panel is open.
  When nothing is visible the hook returns immediately after one atomic check.

### 6.2 How it gets into the game

| Platform / path | Mechanism | Covers |
|---|---|---|
| Windows, DirectX 9/11/12, OpenGL | Launcher starts the game **suspended**, injects `vgames_overlay64.dll` (or `vgames_overlay32.dll` through a small 32-bit helper), then resumes. Hooks via **hudhook** (MIT, Rust; D3D9/11/12, OpenGL 3). | Native Windows games, windowed and exclusive fullscreen |
| Windows, Vulkan | The same DLL registered per user as a Vulkan **implicit layer** (`HKCU\Software\Khronos\Vulkan\ImplicitLayers`), active only when `VGAMES_OVERLAY=1` | Vulkan games |
| Linux, Vulkan (incl. **all Proton games**, since DXVK/VKD3D-Proton render through Vulkan) | `libvgames_overlay.so` as a Vulkan implicit layer in `~/.local/share/vulkan/implicit_layer.d/`, gated by `VGAMES_OVERLAY=1`; pressure-vessel imports it into the Proton container | Native Vulkan + Proton |
| Linux, OpenGL | The same `.so` through `LD_PRELOAD`, hooking `glXSwapBuffers` / `eglSwapBuffers` | Native GL games (and wined3d fallbacks) |
| macOS (native and Wine) | No injection: an `NSPanel` with `.canJoinAllSpaces` + `.fullScreenAuxiliary` and a raised window level is drawn **above fullscreen Spaces** (macOS has no exclusive fullscreen) | Every macOS game |

The Windows DLL and the Vulkan layer share one Rust crate (`crates/vgames-overlay`); MangoHud (MIT) is
the reference for the Vulkan-layer and GL-preload techniques.

### 6.3 Behaviour

- **Toasts** for invites and messages (8 s, stacked top-right, never take input).
- **Panel** on summon: friends online, pending invites with Accept/Decline, recent messages with quick
  reply, "Open vgames". Summon with the global hotkey (default `Shift+F3`, configurable; detected
  in-process while the game has focus) or by holding the controller Guide/PS button for 1 s (read by the
  launcher's controllers module, forwarded over IPC).
- **Input:** on Windows the hook captures keyboard and mouse while the panel is open (WndProc hook) and
  releases them on close. On Linux, where a layer cannot reliably grab input, the panel is driven by
  gamepad and hotkeys; typing a reply switches to the launcher window.
- **Safety valves:** a per-package "In-game overlay" toggle (default on); if a game exits abnormally within
  60 s of launch twice in a row with the overlay on, the launcher disables it for that package and says so
  (one click to re-enable). If injection or layer loading fails, the launch continues without the overlay
  and falls back to an always-on-top window (Windows, X11) or OS notifications (Wayland).
- **Do not disturb** suppresses toasts except invites from favorites.

### 6.4 Known risks

- Remote-thread DLL injection resembles malware behaviour to some antivirus heuristics. The injector,
  helper and DLLs are Authenticode-signed with the launcher's certificate. False positives are reported to
  vendors as part of the release runbook.
- Games with unusual renderers (custom swapchains, D3D11-on-12) may need per-title fixes. The crash
  safety valve keeps them playable meanwhile.
