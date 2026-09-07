# Plan: tapview on Android (eframe + NativeActivity, USB HID via UsbManager)

Modelled on aster's `android/` port: the egui UI is the one the desktop
binary runs, built as a `cdylib` with an `android_main`, wrapped in a thin
Gradle/Kotlin `NativeActivity`, with the platform seam (USB) in Kotlin and
driven from Rust over JNI. Companion to `plans/web.md`: both ports can only
read the touchpad as a **raw HID device**, so most of the work is shared and
is listed there as phase 1.

**Status (2026-09-07):** phases A and C are implemented on branch `android`
(Rust side, Kotlin bridge, Gradle project, Makefile, CI job, nix shell);
phase 0 (the on-device checks) is still open because no phone was attached,
and phase B (pointer capture) was decided against for now.

## Verdict

Feasible, and cheaper than the web port once the shared refactors exist.
Android has neither evdev, libinput nor hidraw for an app, but `UsbManager`
hands a permission-granted `UsbDeviceConnection` for an OTG-attached
touchpad, on which the app can do everything hidraw does and one thing
more:

- `claimInterface(iface, force = true)` detaches Android's `hid-multitouch`
  from the touchpad. From then on the app receives **every** report on the
  interrupt IN endpoint (PTP touch reports: contacts, tip switch,
  confidence/palm, contact size, buttons), and the pad stops moving the
  system pointer — the Enter/Escape grab, for free and unconditional.
- Feature reports go over EP0 (`SET_REPORT`/`GET_REPORT` class requests) —
  heatmap (0x41–0x43) and PTP config (report 3) with the protocol code as
  it is today.
- `GET_DESCRIPTOR(Report)` returns the **raw report descriptor bytes**, so
  the existing byte-level parsers (`config/linux.rs`, `heatmap/discovery.rs`)
  work unchanged. WebHID only gets a parsed `collections` tree; Android does
  not need the web plan's descriptor model to ship, it merely benefits from
  it.
- I/O is blocking on a worker thread, like native. The wasm "main thread
  can never block" constraint does not exist here, so Android does not
  *force* the async `HidDevice`; it fits either shape.

aster's `UsbBridge.kt` already implements exactly this (interface claim by
descriptor prefix, interrupt IN/OUT, feature reports on EP0, the async
permission dance, the plug-in intent filter) — copy the HID half, drop the
CDC-ACM half.

## What Android can and cannot do

| Feature | Native | Android (USB) |
|---|---|---|
| Touch points, trails, buttons, palm | evdev / RawInput | interrupt IN PTP reports → the shared `ptp.rs` parser (`plans/web.md` phase 2.3) |
| Heatmap | hidraw feature reports | same protocol over `controlTransfer` |
| PTP config panel | hidraw + sysfs descriptor | same, descriptor via `GET_DESCRIPTOR(0x22)` |
| libinput panel | libinput / WH_MOUSE_LL | hidden: with the interface claimed nothing interprets the pad |
| Grab | EVIOCGRAB, Enter/Esc | implicit while the device is open; UI text and keys hidden |
| Device enumeration, `--list`, `--device` | udev / SetupAPI | `UsbManager.deviceList` filtered to HID interfaces with a Touch Pad (0x0D/0x05) collection; an in-app picker replaces the CLI |
| Axis extents | evdev absinfo | descriptor logical max (no kernel axis swap involved) |
| `--record` / `--play` | files | record into the app's cache dir + share sheet (later); bundled `testdata/sample.tapv` as demo/no-device mode (same as web) |
| `--info`, `--set-*` | CLI | config panel only |
| Touchpad over **Bluetooth** | evdev | **not via raw HID**: Android reserves HID-over-GATT for the system; apps cannot open the HOGP service of a bonded HID device. See "Pointer capture" below. |

Reach: any arm64 phone/tablet with USB host (OTG) support, Android 8.0+
(`minSdk 26`, android-activity's floor). No root, no special permission
beyond the per-device USB grant.

## Pointer capture (the BLE fallback) — optional, decide separately

Android's own touchpad handling can be observed from an app with
`View.requestPointerCapture()` (API 26+): while captured, the pad's
`MotionEvent`s arrive with `SOURCE_TOUCHPAD`, absolute per-pointer
positions in device units and pressure, and the system pointer freezes.
This works for **any** attached touchpad, Bluetooth included, and needs no
USB permission. It is the only route to a BLE pad on Android.

Limits, checked against winit 0.30.12 (`platform_impl/android/mod.rs`):
winit turns every motion event into a `WindowEvent::Touch` regardless of
source and marks it handled — so captured events reach Rust as egui
`Event::Touch` (id, position, pressure) and never reach Kotlin. Button
presses (`ACTION_BUTTON_PRESS`) and per-pointer geometry (`touchMajor`,
tool type) are dropped by winit. Telling touchpad from touchscreen touches
works: egui's `TouchDeviceId` is a deterministic hash
(`ahash::RandomState::with_seeds(1,2,3,4)`) of winit's `DeviceId(i32)`,
i.e. of Android's `InputDevice` id, which Kotlin can report over JNI along
with name, VID:PID and axis ranges. So: positions + multitouch + pressure,
no buttons, no heatmap, no config. Worth having as a second, degraded
backend for BLE pads, not as the primary design.

## Rejected alternatives

- **Kotlin-native rewrite** (Canvas visualizer, `MotionEvent`s): duplicates
  the renderer, the chip protocol and the descriptor parsing; same reason
  `plans/web.md` rejects a separate JS app. Also caps at what pointer
  capture exposes (no heatmap/config).
- **Pointer capture as the only input**: no buttons, no contact geometry,
  no heatmap; the pad keeps being a pointer everywhere else.
- **Patching winit** to forward source/buttons/geometry: `winit::event::Touch`
  has no fields for them; it would need egui-winit and egui changes too.

## Overlap with the web port

| Piece | Web | Android | Where |
|---|---|---|---|
| One module tree (app in the lib, thin `main.rs`) | needed | needed (the glue crate calls `tapview::android::run_with`) | web phase 1.1 |
| `HidDevice` async / generic | required | optional (thread + `block_on`) | web phase 1.2 |
| Config behind a worker | required | optional (JNI calls block fine on a thread) | web phase 1.3 |
| Descriptor model (`hid/descriptor.rs`) | required (only parsed collections) | not required (raw bytes) | web phase 1.4 |
| Recording over `Write`, `web_time` | required | not required, harmless | web phase 1.5 |
| `Session` + no-session UI (Connect / picker / demo) | required | required | web phase 1.6 |
| `log` instead of `eprintln!` | required | useful (`android_logger`; android-activity already tees stdio to logcat as `RustStdoutStderr`) | web phase 1.7 |
| **PTP input-report parser `ptp.rs`** | required | required | web phase 2.3 |
| Per-target eframe features (no wayland/x11) | wasm | android | both Cargo splits |
| Transport impl | `web_hid.rs` (wasm-bindgen) | `android_hid.rs` (JNI) + `UsbBridge.kt` | port-specific |
| Entry point | `web.rs` + `WebRunner` | `android/rust` cdylib + `NativeActivity` | port-specific |
| Hosting/packaging | trunk + Pages | Gradle + cargo-ndk, APK artifact | port-specific |

Shared work in the web plan's phase 1 that Android strictly needs: 1.1, 1.6,
2.3 (and 1.7 for usable logs). Everything else Android takes if it is there.
Recommended order: do phase 1 from `plans/web.md` first (native-only,
behavior-preserving), then this port — it becomes a transport plus build
plumbing. The Android port ends up exercising the shared `ptp.rs` on real
hardware before the web port does.

**Dev aid worth adding during phase 1:** a Linux `--hidraw` touch backend
that reads PTP input reports from `/dev/hidraw*` through the same `ptp.rs`
(hidraw is a tee, the pad keeps working). It lets the shared parser be
developed and diffed against the evdev backend on a laptop, with no phone or
browser in the loop, and stays useful as a debugging mode.

## Phase 0: on-device spike

Answers the questions only a phone can answer, using the real
`UsbBridge.kt` plus a logcat dump rather than throwaway code:

1. Does `claimInterface(force = true)` succeed on the target phone and
   silence the pad as a pointer? (Stock Android yes; some OEM kernels refuse
   to detach `usbhid`.)
2. Do PTP input reports arrive on interrupt IN afterwards, at the pad's
   native rate?
3. Do feature reports 0x41–0x43 and report 3 round-trip on EP0?
4. Heatmap frame rate: a frame is ~25–40 `GET_REPORT`s, each a JNI call
   plus a control transfer. Expected well above BLE's 1.6 fps, likely near
   USB native.
5. Which VID:PIDs to put in `device_filter.xml` (PixArt 093A:0343 on the
   Framework 13/16; enumerate the 12 and any daisy-attached pad with
   `tapview --list`).

Hardware question to settle in parallel: how a Framework touchpad module is
attached to a phone (USB-C to the module's carrier/daisy board?). If it only
ever shows up over Bluetooth, the USB path is moot and pointer capture is
the whole port.

## Phase A: the Android app

Layout mirrors aster (`android/` at the repo root):

```
android/
  README.md            one-time SDK/NDK setup, build, permissions, debugging
  Makefile             the android-chinese Makefile minus the accessibility
                       targets: build, check, flash, install, run, stop,
                       screenshot, log, crash, connect, pair, devices
  gradlew, gradle/     Gradle 9.2 wrapper (as in android-chinese; aster has none)
  settings.gradle.kts  foojay toolchain resolver (auto-provisions JDK 17)
  build.gradle.kts     AGP 8.13.0, Kotlin 2.2.20
  rust/                tapview-android: cdylib, android_main → tapview::gui::run_with
  app/
    build.gradle.kts   cargoNdk Exec task (cargo ndk -t arm64-v8a build --release → jniLibs)
    src/main/AndroidManifest.xml   NativeActivity, lib_name, configChanges="everything",
                                   USB_DEVICE_ATTACHED intent filter + device_filter.xml
    src/main/kotlin/dev/zoid/tapview/
      TapviewActivity.kt   NativeActivity subclass: insets workaround (as aster), USB attach/detach
      UsbBridge.kt         HID half of aster's bridge: list, open (claim + descriptor), read,
                           setFeature, getFeature, close, status codes
    src/main/res/          NoActionBar theme + v35 edge-to-edge opt-out, adaptive icon
```

Rust side, in the main crate:

1. **Cargo split**: root gains `[workspace] members = ["android/rust"]`;
   eframe declared per target — desktop keeps the defaults, Android gets
   `default_fonts`, `glow`, `android-native-activity` (the glue crate
   re-declares the same set; features only union, so this is what keeps
   wayland/x11 off). `clap`, `evdev`, `udev`, `input`, `libc` are already
   or become target-gated. Android-only deps: `jni`, `log`,
   `android_logger` (glue crate).
2. **`android_hid.rs`** (`cfg(target_os = "android")`): the Rust half of
   the bridge, after aster's — `init(vm, activity)` from `android_main`,
   `list_devices()`, `open()`, an `AndroidHidDevice: HidDevice`
   (`set_feature`/`get_feature` → EP0), and a reader thread doing interrupt
   IN reads → `ptp::parse` → `Sender<TouchState>`. Errors for
   permission-pending/gone map to a retryable UI message.
3. **Session wiring**: `Session::from_android_device(path)` does what
   `main.rs` does today for a Linux devnode — descriptor → extents +
   `PtpConfig` + burst length → spawn reader, heatmap and config workers.
4. **UI arms**: the no-session screen shows the USB picker (or "plug a
   touchpad into the USB-C port"), the status line gets an Android arm,
   grab keys/text and the libinput panel are cfg'd out. Safe-area insets
   read over JNI and padded, as aster does (winit 0.30 ignores insets).
5. **Logging**: `android_logger` tagged `tapview`; panics hooked into
   logcat.

Kotlin side: `UsbBridge.kt` as above; `TapviewActivity` additionally
listens for `USB_DEVICE_DETACHED` so the session can be torn down cleanly
(Rust side sees read errors either way).

## Phase B (optional): pointer-capture backend

Only if BLE pads matter on Android. Small and independent of phase A:

- `InputBridge.kt`: `touchpads()` (id, name, VID:PID, X/Y motion ranges,
  external/Bluetooth flags), `requestCapture()`/`releaseCapture()` posted
  to the UI thread, `hasCapture()` from `onPointerCaptureChanged`; auto-capture
  on window focus when a touchpad exists.
- Rust: an `eframe::App` wrapper using `raw_input_hook` to pull
  `Event::Touch`es whose device id hashes to the touchpad, convert them to
  `TouchState` (slot table keyed by touch id, position × `pixels_per_point`
  back to device units, extents from the motion ranges), and strip them and
  their emulated pointer events so egui widgets do not react to touchpad
  taps. Touchscreen touches are visualized when no pad is captured.
- UI: source label ("USB raw HID" / "captured touchpad" / "touchscreen"),
  capture toggle.

## Phase C: ship

- **CI** (`.github/workflows/ci.yml`, new `check-android` job, like aster's):
  `rustup target add aarch64-linux-android`, `sdkmanager "ndk;27.2.12479018"`
  (runner has the SDK, pins no NDK), `cargo install cargo-ndk` (cached by
  rust-cache), `setup-java` 17 + `gradle/actions/setup-gradle`,
  `cargo clippy --lib --target aarch64-linux-android -- -D warnings` (covers
  the JNI code no other job compiles), `make -C android build`, upload
  `app-debug.apk`. Add `tags: ['v*']` to the push trigger and attach the APK
  to the GitHub release on tags (`gh release create` if missing, then
  `upload --clobber`, `permissions: contents: write`), as android-chinese
  does. The existing Linux/Windows jobs build the root package only and are
  unaffected by the workspace member.
- **README**: "On Android" section — USB-OTG, the permission grant, what is
  missing (libinput panel, Bluetooth pads unless phase B), `make -C android
  flash`, logcat.
- **flake.nix**: `aarch64-linux-android` target and `cargo-ndk` in a
  `devShells.android`; the NDK/SDK stay outside nix (`~/Android`, as today).
- **.gitignore**: `android/.gradle`, `android/app/build`,
  `android/app/src/main/jniLibs`, `android/local.properties`, `android/build`.

## Local toolchain (already present)

`~/Android` is a full SDK root with `ndk/27.2.12479018`, `platforms`,
`build-tools`, `cmdline-tools` and `platform-tools`; `~/Android/gradle-9.2.1`;
JDK 17 cached under `~/.gradle/jdks`; `cargo-ndk` 4.1.2 and the
`aarch64-linux-android` target installed. No phone was attached while this
was written, so the APK can be built here but only the user can run phase 0.

## Validation

Phase A, on a phone with a USB-attached Framework pad:
- Plugging in offers tapview; the pad stops moving the pointer once the
  app opens it, and resumes when the app closes.
- Touches match the Linux view side by side (positions, palm colouring,
  buttons, contact count), including 5 fingers.
- Heatmap renders; frame rate noted in `android/README.md`.
- Config panel shows what `tapview --info` prints on Linux for the same
  pad; a haptic-intensity write sticks.
- Unplug mid-session → error state with a retry, no hang, no ANR.
- Rotation and an attached keyboard/pad flipping `touchscreen|keyboard`
  config do not recreate the activity (the `configChanges` list).
- `make -C android log` shows Rust logs and would show a panic.

CI: the `check-android` job produces an installable debug APK on every
push; a `v*` tag attaches it to the release.
