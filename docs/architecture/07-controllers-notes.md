# 07 — Controller implementation notes (PLAY-04 / PLAY-05)

This additive contract completes [07-controllers.md](07-controllers.md). Existing
manifest kinds, emulation decisions, mapping defaults and `ControllerEvent` stay
compatible. PLAY owns commands, pending IPC entries, mocks and their implementation.

## 1. Input service and identity

SDL3 is built from source and linked statically on Windows, Linux and macOS. A
dedicated `vgames-sdl` thread owns SDL initialization and gamepad handles, enables
the HIDAPI drivers and loads the bundled `gamecontrollerdb.txt`. Initialization
failure is reported to Settings; it never prevents a native game launch.

The thread consumes events with **`SDL_WaitEvent`**, replacing §2's
`SDL_WaitEventTimeout`. Shutdown pushes a private SDL event. With no connected pad
there must be no recurring timer, polling or periodic wakeup. Navigation repeat and the
one-second Guide hold use deadlines armed only while a relevant input is held;
release, disconnect and shutdown cancel them. They wake the event queue, rather
than changing it into an idle polling loop. Tests measure the input loop and the
named thread, including the disconnected idle state.

**Measured headless SDL caveat:** SDL 3.4.16's `SDL_WaitEvent` falls back to an
internal one-millisecond pump loop without an active SDL video window. The local
Ubuntu 24.04 no-pad probe recorded 55,526 voluntary context switches in 60 seconds
and 0.78 s CPU, despite an unchanged Rust event-loop counter. That counter alone
cannot establish idle blocking. The implementation must gate headless queue
consumption with a real blocking event notification, retain native device-change
notifications for hot-plug, and pass `perf/idle.py --thread vgames-sdl`'s actual
context-switch measurement before shipment. It must not replace this check with
a Rust iteration counter or a periodic discovery timer.

The proposed no-pad adapters use Linux device-node notifications plus eventfd,
Windows device-notification messages on the SDL owner thread plus an event, and
macOS IOKit/CFRunLoop sources. A private control event is enqueued before the
native wake signal, so waking cannot outrun queue insertion. Notifications are
registered before SDL enumerates devices. The local updated SDL 3.4.16 probe
records 0 voluntary switches and 0.00 s CPU over 60 s; native hosted validation
and real device recognition remain required before PLAY-04 is complete.

An instance id identifies one connection, not a persisted physical device. Each
pad exposes `instance_id`, `kind` (`ControllerKind`), `name`, `connection`
(`usb | bluetooth | unknown`), `battery` (nullable percentage plus
`charging | on_battery | wired | unknown`) and nullable `player_index` (zero-based).
Unknown battery/transport information is displayed as unknown, never guessed.
Disconnect releases handles, held navigation actions, tester state and virtual
output; reconnect obtains a new instance id. Player order follows SDL's assigned
player index where available and otherwise connection order.

## 2. Settings and tester

Settings → Controllers lists pads, driver availability and a live tester. The
screen has loading, empty, unavailable and error states, keyboard access and
gamepad navigation. "Use Nintendo button labels" is a persisted global setting;
mapping remains by position by default (07 §5).

The command surface is:

| Command | Behavior |
|---|---|
| `controllers_list()` | Current pads and input-service availability. |
| `controllers_settings_get()` / `controllers_settings_set(settings)` | Read/write the global Nintendo-label preference. |
| `controllers_tester_start(instance_id)` | Select a connected pad and return a tester subscription id. |
| `controllers_tester_stop(subscription_id)` | Idempotently release that subscription. |
| `controllers_profile_get(pkg)` / `controllers_profile_set(pkg, profile)` | Read/write the per-package profile in `controller_profiles`. |
| `controllers_driver_status()` | Platform-specific virtual-pad and physical-hide availability. |

`pkg` is the existing server/package reference. Profiles contain `mode`
(`auto | always | never`) and the existing mapping profile: button remaps,
per-stick radial deadzone/anti-deadzone and axis inversion. Defaults match the
current `controllers::mapping` and `decision` modules. Reject unknown buttons,
duplicate source remaps and deadzone/anti-deadzone percentages outside 0–99; no partial
write on failure. Saving is transactional and survives launcher restart. The
launch hook snapshots the profile so editing Settings cannot corrupt an active
session. A missing profile means the default, never an error.

The tester emits canonical button/axis state with subscription and instance ids,
at most 30 updates per second and only while a live subscription exists. Closing
or leaving the screen stops it; disconnect also closes it. Dropped or replaced
subscriptions never receive another pad's state. Tester subscriptions have no
effect on game input and never enable emulation.

## 3. Events and navigation

`ui-nav` retains `{action, controller, repeat}` from `contract/core.ts`, including
directions, accept/back/menu/options and tab previous/next. Press emits once;
only directions repeat after an initial delay. Stick navigation uses hysteresis
to prevent chatter. Release and hot-unplug reset debounce, repeat and held state.

`active-controller-changed` retains `{controller: ControllerKind | null}` for
glyph selection. It changes on meaningful input, not jitter or a periodic scan.
`pads-changed` tells the UI to refresh the pad list; the tester uses its own scoped
stream. Existing bus events `Connected`, `Disconnected` and `GuideHeld` remain;
Guide/PS held for one second emits `GuideHeld` once per hold. Pending navigation
entries are removed only after equivalent generated Rust events exist.

## 4. Driver states and launch behavior

Driver status distinguishes available, unavailable and probe-error states;
unavailability is a normal Settings state with platform-specific help:

| Missing requirement | State | Help / resulting behavior |
|---|---|---|
| Windows ViGEmBus | `vigembus_missing` | Link to the official ViGEmBus project/installer; emulation stays off. |
| Linux uinput access | `uinput_denied {udev_rule}` | Show the bundled `uaccess` rule and installation steps; emulation stays off. |
| Windows physical-pad hiding | `hidhide_missing` | Link to HidHide; explain possible double input and the SDL VID/PID fallback. |
| macOS restricted entitlement | `macos_entitlement_missing` | Explain 07 §4.1 and Apple's restricted entitlement; current unsigned builds keep CoreHID off. |

Help URLs are fixed application links and go through the existing validated
external-URL command. Driver status contains no arbitrary shell commands and
does not install drivers or request privilege automatically. PLAY-04 mocks these
states; PLAY-05 replaces them with native probes and virtual-pad backends.

The PLAY-05 `LaunchHooks` implementation uses the existing decision function.
Proton/Wine always use passthrough. Emulation exists only during a running
session, passes rumble back, supports hot-plug and releases physical grabs on
exit/abort. Backend failure disables that pad's emulation with a notice and
does not veto the launch.

## 5. Launch identity and verified controller configuration

LaunchContext gains a unique session_id for each launch and re-attachment, plus
the controller configuration from the signed manifest that passed pre-launch
verification. A hook never re-parses an unverified install file to choose an
emulation target. Profile and controller configuration are captured per session.

Controller resources are keyed by session_id, not package alone: delayed exit
work from a previous run cannot destroy a newly prepared run of the same package.
Abort and confirmed-tree-exit teardown are idempotent for that identity. A
reattached game gets a fresh local identity; restore virtual output after input
initialization without rerunning save pull or process-injection hooks.
