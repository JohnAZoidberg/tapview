# Tapview on Android

The touchpad visualizer as an Android app, for a phone or tablet with a
USB-C (OTG) port: plug a touchpad in and see its contacts, heatmap and PTP
configuration without a laptop. The egui UI is the exact one the desktop
binary runs — it lives in the main crate — with the platform differences
confined to one seam:

- **USB.** Android has no `/dev/input/event*` or `/dev/hidraw*`; USB goes
  through `UsbManager` and a user-granted connection. The transfers live in
  Kotlin ([`UsbBridge.kt`](app/src/main/kotlin/me/danielschaefer/tapview/UsbBridge.kt))
  and `src/android_hid.rs` in the main crate drives them over JNI: raw PTP
  touch reports on the interrupt endpoint (decoded by `src/ptp.rs`, the same
  parser `tapview --hidraw` uses on Linux), feature reports for the heatmap
  and the config panel on EP0.
- Claiming the pad's HID interface **detaches Android's own touchpad
  driver**: while tapview has the device open, the pad stops moving the
  system pointer and the app receives every report — the Linux "grab",
  unconditionally. Closing the app (or the session) gives the pad back.

What is missing compared to the desktop: the libinput/interpreted-input
panel (nothing interprets the pad while it is claimed), recording to a
file, and Bluetooth touchpads (Android reserves HID-over-GATT for the
system; see `plans/android.md` for the pointer-capture alternative).

Layout:

- `rust/` — the `tapview-android` crate: a `cdylib` whose `android_main`
  wires logging and the USB bridge, then calls `tapview::android::run_with`.
  This crate (not `tapview`) owns the winit/android-activity dependency.
- `app/` — the Gradle module: `TapviewActivity` (a `NativeActivity`), the
  USB bridge, and the `device_filter.xml` that offers the app (and silently
  grants USB access) when a known touchpad is plugged in.

## One-time setup

```sh
# Rust target
rustup target add aarch64-linux-android
cargo install cargo-ndk

# Android SDK + NDK (any install works; sdkmanager shown). ANDROID_HOME can
# be any directory, e.g. ~/Android.
curl -LO https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip
unzip commandlinetools-linux-*.zip -d $ANDROID_HOME  # then move into cmdline-tools/latest/
yes | $ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager --licenses
$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager \
    "platform-tools" "platforms;android-35" "build-tools;35.0.0" "ndk;27.2.12479018"
```

A Java runtime to launch Gradle with is also needed (the build itself
auto-provisions the JDK 17 it compiles with, via the foojay toolchain
resolver, so any Java ≥ 17 is fine). Gradle comes with the repository
(`./gradlew`).

## Build and install

The Makefile in this directory wraps the whole day-to-day loop (`make help`
lists everything; adb/ANDROID_HOME are auto-discovered):

```sh
cd android
make pair CODE=123456   # once per machine: wireless debugging pairing
make connect            # attach over Wi-Fi
make flash              # build + install + launch
make screenshot         # grab the phone's screen, prints the path
make log                # follow crashes/ANRs/Rust logs
```

Or by hand:

```sh
cd android
ANDROID_HOME=... ./gradlew assembleDebug     # or: installDebug, with a phone on adb
```

Gradle runs `cargo ndk -t arm64-v8a build --release` itself (the `cargoNdk`
task) and packages the resulting `libtapview_android.so`. To iterate on just
the Rust side:

```sh
cd android/rust
ANDROID_HOME=... cargo ndk -t arm64-v8a -o ../app/src/main/jniLibs build --release
```

The Rust side can also be linted without any Android SDK at all:

```sh
make check    # = cargo clippy --lib --target aarch64-linux-android, from the repo root
```

## Using it

Plug the touchpad into the phone. If it is a known pad (`device_filter.xml`),
Android offers to open Tapview for it; picking it (and checking "always")
grants USB access silently and the app opens the pad straight away. For any
other pad, the device list shows it with a **Request access** button; grant
the dialog and the pad is opened. With exactly one touchpad attached it is
opened automatically. **Play demo recording** shows a bundled session from a
Framework 13 pad when nothing is attached.

Unplugging mid-session drops back to the device list with a message.

## USB permissions

Android gates USB access per device, per app. An open of an ungranted
device posts the system permission dialog and the app says to retry after
granting; the `device_filter.xml` intent filter makes the grant automatic
when the user picks this app on plug-in and ticks "always".

## Debugging

Rust logs and panics go to logcat: `adb logcat -s tapview RustStdoutStderr`
(or `make log`).

## Known limitations

- winit 0.30 exposes no safe-area insets; the activity reports them over
  JNI and the UI pads for them. If something still hides under a system
  bar, that padding is the place to look.
- Some OEM kernels refuse to detach `usbhid` from a device
  (`claimInterface` fails); the app then reports "could not detach the
  system driver".
- Heatmap frame rate over USB has not been measured yet; each frame is
  ~25–40 GET_REPORT control transfers, each a JNI call.
