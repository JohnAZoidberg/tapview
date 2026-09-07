# Tapview

A Linux touchpad visualizer. Shows multitouch contact points in real time using the kernel's MT Protocol B events. Useful for debugging touchpad behavior, testing palm rejection, and understanding how your touchpad reports touches.

## Tested on

| Hardware            | Raw Events | libinput | Heatmap |
|---------------------|------------|----------|---------|
| Framework Laptop 12 | Working    | Working  | Working |
| Framework Laptop 13 | Working    | Working  | Working |
| Framework Laptop 16 | Working    | Working  | Working |

## What it does

- Discovers your touchpad automatically via udev
- Reads raw multitouch events from `/dev/input/event*`
- Renders touch points as colored circles with trails
- Magenta = first finger, teal = additional fingers, gray = palm-rejected touches
- Shows press state (filled dot) and double-tap state (ring)
- Optionally grabs exclusive access so touches don't move the system cursor

## Dependencies

### Build dependencies

You need a Rust toolchain and development headers for libudev and libinput:

**Fedora / RHEL:**
```
sudo dnf install libudev-devel libinput-devel
```

**Debian / Ubuntu:**
```
sudo apt install libudev-dev libinput-dev
```

**Arch:**
```
sudo pacman -S systemd-libs libinput
```

You also need the standard graphics libs that eframe/egui depend on (typically already present on desktop systems):

**Fedora:**
```
sudo dnf install gcc pkg-config libxkbcommon-devel wayland-devel libX11-devel
```

**Debian / Ubuntu:**
```
sudo apt install libxkbcommon-dev libwayland-dev libx11-dev
```

**NixOS/Nix:**
```
nix develop
```

Or run directly without cloning:
```
# Without cloning
sudo nix run github:JohnAZoidberg/tapview
sudo nix run github:JohnAZoidberg/tapview -- -l --heatmap

# In cloned repo
sudo nix run
sudo nix run . -- -l --heatmap
```

### Runtime

Requires read access to the touchpad's `/dev/input/event*` device. Typically this means running as root or adding your user to the appropriate groups:

```bash
# For /dev/input/event* access (evdev)
sudo usermod -aG input $USER

# For /dev/hidraw* access (heatmap feature)
sudo usermod -aG hidraw $USER
```

Log out and back in for group changes to take effect. After this, you can run tapview without sudo.

## Building

```
cargo build --release
```

The binary will be at `target/release/tapview`.

## Usage

```
sudo ./target/release/tapview [OPTIONS]
```

### Options

| Flag | Description |
|------|-------------|
| `-t, --trails <N>` | Number of trail frames to show (default: 20, max: 20) |
| `-v, --verbose` | Print raw kernel multitouch events to stderr |
| `-l, --libinput` | Show libinput pointer/scroll/gesture data in a right side panel |
| `--record <path>` | Record touch session to a binary file |
| `--play <path>` | Play back a recorded touch session (no device needed) |
| `--hidraw` | Linux: read touches from the hidraw node through tapview's own PTP report parser instead of evdev (same `hidraw` access as the heatmap; no grab). For checking the parser against the kernel |
| `--dump-hidraw <N>` | Linux: print the HID report descriptor and N raw input reports as hex, then exit |
| `-h, --help` | Show help |

### Controls

| Key | Action |
|-----|--------|
| Enter | Grab touchpad (exclusive access, system cursor stops moving) |
| Escape | Release grab |
| Space | Play/pause (playback mode) |
| Left/Right | Step -/+100ms (playback mode) |

### Examples

```
# Basic usage
sudo ./target/release/tapview

# Short trails
sudo ./target/release/tapview --trails 5

# Debug raw events
sudo ./target/release/tapview --verbose

# Compare raw events with libinput interpretation
sudo ./target/release/tapview --libinput

# Record a touch session
sudo ./target/release/tapview --record /tmp/session.tapv

# Play it back (no device/sudo needed)
./target/release/tapview --play /tmp/session.tapv
```

#### Cross-platform builds with Nix

```
nix build            # Linux build
nix build .#windows  # Windows cross-compile
```

#### Windows clippy

```
nix develop .#windows -c cargo clippy --target x86_64-pc-windows-gnu
```

#### Incremental builds with Nix

Build as your user, then run the binary with sudo:

```
nix develop -c cargo build && nix develop -c bash -c 'sudo env LD_LIBRARY_PATH="$LD_LIBRARY_PATH" ./target/debug/tapview --record /tmp/test.tapv'
```

## On Android

Tapview also builds as an Android app for phones and tablets with a USB-C
(OTG) port: plug a touchpad in and the same UI shows its contacts, heatmap
and PTP configuration. The pad is read as a raw USB HID device (Android has
no evdev or hidraw for apps), which also takes it away from the system
pointer while the app has it open. Bluetooth pads and the libinput panel are
not available there. Build and install with

```
make -C android flash
```

See [android/README.md](android/README.md) for the one-time SDK/NDK setup,
the Makefile targets and the USB permission flow.

## In the browser

Tapview also runs as a web page: the same app compiled to WebAssembly, with
the touchpad reached through
[WebHID](https://developer.mozilla.org/en-US/docs/Web/API/WebHID_API)
instead of evdev and hidraw (`src/web_hid.rs`, see `plans/web.md`). Every
push to `main` deploys it to GitHub Pages:
<https://johnazoidberg.github.io/tapview/>. To run it locally:

```
rustup target add wasm32-unknown-unknown
cargo install trunk   # or download a release binary; nix: `nix develop .#web`
trunk serve           # http://localhost:8080 (localhost is a secure context)
```

`trunk build --release` writes the same static site to `dist/`, which any
file server can host (`python -m http.server -d dist`) — just not `file://`,
and anywhere beyond localhost must be HTTPS or WebHID stays disabled.

Browser differences, all inherent to the platform:

- **Chromium on the desktop only** (Chrome, Edge, Opera): no other browser
  ships WebHID. Elsewhere the page can still play the bundled demo recording.
- **Linux needs hidraw access.** Chrome opens `/dev/hidraw*` as the user, so
  the same `hidraw` group membership the heatmap needs applies — and only
  that; the `input` group is not needed.
- **The touchpad is granted, not discovered.** Press **Connect touchpad** and
  pick it in the browser's prompt. The grant persists across visits (the page
  re-takes it on load and opens the pad when it is the only one granted).
  **Disconnect** in the bar above a live view returns to the list, where
  another granted pad can be opened or a new one connected. Windows may
  refuse to open the touchpad collection at all; Linux is the tested
  platform.
- **Touches come from the raw PTP reports**, parsed like on Android, so the
  system cursor keeps working and palms show up as the firmware flags them.
  Heatmap and PTP configuration work as on the desktop.
- **No grab, no libinput.** A page cannot take exclusive hold of the pad. The
  right-hand panel shows what the browser makes of it instead: pointer
  motion, scrolling, and pinch (delivered as ctrl+wheel), only while the
  pointer is over the page.
- **No recording to a file yet.** Playback of the bundled demo works.

## Architecture

Two-thread design:

- **Input thread** reads evdev events in a non-blocking loop, processes them through an MT Protocol B state machine, and sends touch snapshots to the UI thread over an `mpsc` channel.
- **UI thread** runs the eframe/egui event loop, drains the channel each frame, and renders touch points with trails.

```
src/
  main.rs              CLI, device discovery, thread spawn, eframe setup
  app.rs               eframe::App impl, rendering loop, history buffer
  multitouch.rs        MT Protocol B state machine (platform-independent)
  dimensions.rs        Touchpad-to-screen scaling math
  render.rs            egui Painter drawing helpers
  libinput_backend.rs  Libinput library integration (pointer, scroll, gestures)
  libinput_state.rs    Libinput event state for visualization
  input/
    mod.rs             InputBackend trait
    evdev_backend.rs   Linux evdev implementation
  discovery/
    mod.rs             DeviceDiscovery trait
    udev_discovery.rs  Linux udev implementation
```

The trait-based design (`InputBackend`, `DeviceDiscovery`) is intended for future extensibility to other platforms or input sources.
