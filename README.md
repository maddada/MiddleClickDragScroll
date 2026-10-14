# MiddleClickDragScroll

Hold the middle mouse button and drag to scroll, like two-finger scrolling on a trackpad,
with flick-to-glide momentum. Lives in the notification area.

## Download

Get `MiddleClickDragScroll.exe` from
[Releases](https://github.com/maddada/MiddleClickDragScroll/releases/latest) and run it; there's
nothing to install. The exe isn't code-signed, so SmartScreen may warn the first time: click
**More info → Run anyway**.

## Using it

| You do | What happens |
|---|---|
| Hold middle button and drag | Content follows your hand (natural direction by default); the pointer stays put unless you turn off "Freeze cursor while scrolling" |
| Release while moving (flick) | Content glides and slows down smoothly |
| Stop, then release | Stops dead, no glide |
| Click / real wheel during a glide | Glide stops |
| Flick again in the same direction | Speed stacks |
| Tap middle button | Normal middle click (sent on release) |

Tray icon: **left-click** toggles on/off, **right-click** for the menu (Settings, Start with
Windows, Restart as administrator, Quit). Launching the exe again opens the settings window.

The settings window has a slider for every tunable, a glide preview, an excluded-apps list
(apps where the middle button behaves normally, e.g. `blender.exe`) and **Copy settings**,
which copies all values as text.

Settings live in `%APPDATA%\MiddleClickDragScroll\settings.ini` (plain `key = value`),
next to `log.txt`.

### Administrator mode

Windows blocks non-elevated apps from scrolling elevated windows (Task Manager, admin
terminals…). On launch the app offers to restart as administrator. If you accept, that's
remembered, and "Start with Windows" then uses a scheduled task with highest privileges,
so there's no UAC prompt at sign-in.

## Building

```
cargo build --release
target\release\MiddleClickDragScroll.exe
```

Needs the MSVC toolchain and the Windows SDK (for `rc.exe`, used to embed the icon, manifest
and version info). Debug builds keep a console window for logs; release builds don't.
`cargo test` runs the platform-independent tests (gesture recognizer, physics, settings).

## Code layout

```
src/
  main.rs              entry point
  shared/              OS-independent, reused by every port
    settings.rs        Settings struct, slider/toggle table (drives the UI and the file), persistence
    gesture.rs         middle-button state machine: tap vs drag, what to swallow
    engine.rs          scroll engine thread: drag accumulation, flick velocity, glide physics
    icon.rs            procedural icon (also used by build.rs to make the .ico)
    log.rs             log file + panic hook
  ui/                  settings window (egui), shared across OSes
  platform/
    mod.rs             the contract each OS implements (SystemServices, run())
    windows/
      hook.rs          WH_MOUSE_LL hook -> gesture recognizer
      inject.rs        ScrollOutput: SendInput high-resolution wheel events
      vsync.rs         FrameClock: waits for the monitor's vblank (D3DKMT), timer fallback
      tray.rs          notification icon, menu, main message loop
      elevation.rs     admin detection, launch prompt (TaskDialog), elevated relaunch
      autostart.rs     Run key / elevated scheduled task
      instance.rs      single-instance lock
build.rs               Windows resources (icon, manifest, version info)
```

Data flow: OS input hook → `shared::gesture` (decides swallow/pass, emits commands) →
`shared::engine` thread (physics, paced by a `FrameClock`) → platform `ScrollOutput`.

The engine works in logical pixels: `+x` reveals content to the right, `+y` reveals content
below. Each backend converts to its native units (Windows: 120 wheel units per notch,
normalised by the system's lines-per-notch setting).

## Porting notes

A new OS needs a `platform/<os>/` module providing `run()`, an input hook, a `ScrollOutput`,
a `FrameClock` and a `SystemServices`. Everything in `shared/` and `ui/` is reused.

**macOS**
- Input: `CGEventTapCreate` on other-mouse-down/up/dragged (button 2). Returning NULL swallows
  the event. Needs the Accessibility permission (macOS's equivalent of the admin prompt).
- Freeze cursor: `CGAssociateMouseAndMouseCursorPosition(false)`, and read
  `kCGMouseEventDeltaX/Y` for deltas.
- Output: `CGEventCreateScrollWheelEvent2` with `kCGScrollEventUnitPixel`. Set
  `kCGScrollWheelEventScrollPhase` / `MomentumPhase` (use `momentum` and `glide_started`)
  so apps latch the target and rubber-band natively.
- Frame clock: `CVDisplayLink` / `CADisplayLink`.
- Tray: `NSStatusItem`. Autostart: `SMAppService.mainApp`.
- winit must run on the main thread there, so the settings window can't use the
  dedicated-thread approach in `ui/settings_window.rs`. Run the NSApp/status item and
  eframe on the main thread instead.

**Linux**
- Input: read and grab (`EVIOCGRAB`) the mouse's evdev device, then re-emit everything except
  the gesture through a uinput virtual mouse. This works on X11 and Wayland. Needs the
  `input` group or a udev rule.
- Output: uinput `REL_WHEEL_HI_RES` / `REL_HWHEEL_HI_RES` (120 per notch).
- Frame clock: a timer at the monitor refresh rate is fine.
- Tray: StatusNotifierItem (e.g. the `ksni` crate). Autostart:
  `~/.config/autostart/*.desktop`.
- eframe needs its `x11` / `wayland` features enabled for that target.

## Limitations

- Holding the middle button is consumed by the gesture, so apps that need a middle-button
  *hold* (Blender orbit, CAD pan, browser autoscroll) should go in Excluded apps.
- How smooth scrolling looks depends on the app. Browsers, Electron apps and most modern
  apps take pixel-precise wheel deltas; old Win32 apps that only understand whole notches
  scroll in steps.
- Windows has no user-mode way to fake touchpad input. That would need a signed virtual HID
  driver. Instead, "Stick to the starting area" makes plain wheel events act like a touchpad
  gesture. Once the pointer moves off the starting spot, wheel messages are posted to the
  original window carrying the original position, so the app keeps scrolling the same area.
  This works in browsers, Electron, Qt and most Win32 apps. UWP/WinUI apps ignore posted
  wheel messages, so they get normal wheel input while the pointer is over them, and
  scrolling pauses while it's elsewhere. Apps that hit-test the live cursor instead of the
  message position (e.g. WPF) scroll whatever is under the pointer.
- "Stop glide on pointer move" defaults to 250 px: moving the mouse that far after a flick stops
  the glide. Drag it all the way right ("never") to keep gliding wherever the pointer goes, as
  on a trackpad.

## See also

- [AltMirroredResize](https://github.com/maddada/AltMirroredResize): hold Alt and drag a window
  edge to resize it from both sides at once, like Option-resizing on macOS.
- [TopScrollBrightness](https://github.com/maddada/TopScrollBrightness): scroll with the pointer at
  the top edge of a screen to change that monitor's brightness.
- [Ghostex](https://ghostex.dev) ([GitHub](https://github.com/maddada/Ghostex)): a native
  workspace for coding agents, built in Rust with GPUI.
