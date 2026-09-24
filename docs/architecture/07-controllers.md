# 07 — Controllers

Goal: any common controller works in any package. If a package supports only Xbox
(XInput) pads, a DualSense or Switch Pro controller shows up to the package as an
Xbox 360 pad, with rumble passed back.

## 1. Pipeline

```
 physical pads ──▶ SDL3 (input thread) ──▶ canonical state ──▶ mapping profile ──▶ output backend
 (USB/Bluetooth)    HIDAPI drivers:         (SDL gamepad       (per package,       ├ passthrough (no-op)
                    DualSense, DS4,         layout: buttons,   user overrides,     ├ Windows: ViGEmBus virtual X360/DS4
                    Switch Pro, Xbox,       axes, triggers,    deadzones,          ├ Linux:   uinput virtual X360
                                                                                          └ macOS:   CoreHID virtual pad (entitled builds)
                    generic via             gyro optional)     remaps)
                    gamecontrollerdb        ◀────────── rumble / LED feedback from the virtual pad ──────────┘
```

## 2. Detection

- SDL3 (statically linked) runs on a dedicated thread, event-driven (`SDL_WaitEventTimeout`),
  with the HIDAPI drivers enabled for PlayStation, Nintendo and Xbox controllers.
- Bundle `gamecontrollerdb.txt`, updated on each launcher release.
- Each pad is classified as `xinput` (Xbox family), `dualshock4`, `dualsense`, `switch_pro`, or `generic`.
  The kind, name, connection (USB/BT), battery and player index go to the UI (Settings → Controllers)
  with a live input tester.
- Hot-plug is supported at any time, including mid-game (virtual pads are created/destroyed accordingly).

## 3. Emulation decision (per launch)

Inputs: the manifest's `controllers.supported` and `emulate_as`, the connected pads, and the user override
(Settings → per-package: "Automatic / Always emulate / Never emulate").

```
for each connected pad:
  if launch runs through Proton or Wine (09-compatibility)                  → passthrough (the layer maps pads to XInput)
  if pad.kind ∈ supported or supported is empty/absent or user said "never" → passthrough
  else if emulate_as is set and a backend is available                     → virtual pad of emulate_as
  else                                                                      → passthrough + one-time notice
```

- Virtual pads exist only while the package's process tree is running.
- **Hide the physical pad** from the game when emulating (otherwise the game sees two pads):
  - Windows: HidHide if installed (whitelist the launcher, cloak the physical device for the
    session). Otherwise start the game with `SDL_GAMECONTROLLER_IGNORE_DEVICES` set for the
    physical VID/PID (covers SDL-based games) and warn about possible double input.
  - Linux: grab the physical evdev node (`EVIOCGRAB`) for the session and set
    `SDL_GAMECONTROLLER_IGNORE_DEVICES` for the child.
- Player order is preserved (first physical pad → first virtual pad).

## 4. Backends

| Platform | Backend | Notes |
|---|---|---|
| Windows | **ViGEmBus** via `vigem-client` | Kernel driver. Upstream retired in 2023 but still working and widely installed; a successor ("VirtualPad") is announced. Isolate it behind `trait VirtualPadBackend` so it can be swapped. If the driver is missing, Settings → Controllers offers the official installer link with an explanation; emulation stays off until it is installed. |
| Linux | **uinput** via `evdev::uinput` | Create an Xbox 360 device (vendor `045e`, product `028e`, correct axis ranges and FF_RUMBLE) so SDL, Wine/Proton and native games map it correctly. Needs `/dev/uinput` access: ship a udev rule in the packages (`uaccess` tag) and explain when it is missing. |
| macOS 15+ | **CoreHID `HIDVirtualDevice`** through a small Swift bridge (C ABI) | Requires Apple's **restricted** entitlement `com.apple.developer.hid.virtual.device` (requested by humans in the Apple Developer portal; paid program; no development variant yet). Emulate a pad with a known SDL GameControllerDB mapping, as Sunshine's macOS virtual-gamepad work does (it emulates a Razer Serval). Builds without the entitlement ship with the backend disabled and the UI explains why. |

## 4.1 macOS: why emulation is rarely needed

- **Native Mac games** read controllers through Apple's Game Controller framework, which natively supports
  Xbox, PlayStation (DualShock 4, DualSense) and Nintendo (Switch Pro, Joy-Con) controllers plus MFi pads.
  A DualSense already works in a game that "only supports Xbox" if the game uses that framework.
- **Windows games under Wine** (09-compatibility §3) see controllers through Wine's own XInput
  translation, so no virtual pad is needed.
- The CoreHID backend therefore targets the remaining case: SDL- or IOHID-based native games with
  limited mapping databases. It is optional, not a launch blocker.

## 5. Mapping

- Canonical → XInput: A/B/X/Y by **position** (south/east/west/north), so Cross → A and
  Circle → B. Switch Pro follows position too (the physical B, bottom, → A). Setting "Use
  Nintendo button labels" swaps by label instead.
- Triggers: analog 0–255 (from SDL 0–32767). Sticks: SDL range → XInput ±32767 with
  per-stick radial deadzone (default 8%) and anti-deadzone 0%.
- DualSense touchpad click → Back/View, PS → Guide (Guide/PS is also watched for the overlay chord,
  05-social §6). Gyro not mapped in v1.
- Rumble: virtual pad force feedback → `SDL_RumbleGamepad` on the physical pad (large/small motors).
- Per-package user profiles (remaps, deadzones, invert Y) are stored locally, exportable as JSON.

## 6. Performance and robustness

- Added latency < 2 ms p99: forward on every SDL event (no fixed polling loop), no allocation per event.
- The input thread never blocks on I/O. Backend errors disable emulation for that pad with a notice
  and never crash the launcher.
- Soak test: 8 h with 2 emulated pads under constant input shows no RSS growth and no dropped
  hot-plug events.
