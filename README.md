# Tapview

A Linux touchpad visualizer. Shows multitouch contact points in real time using the kernel's MT Protocol B events. Useful for debugging touchpad behavior, testing palm rejection, and understanding how your touchpad reports touches.

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

### Runtime

Requires read access to the touchpad's `/dev/input/event*` device. Typically this means running as root or being in the `input` group.

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
| `-h, --help` | Show help |

### Controls

| Key | Action |
|-----|--------|
| Enter | Grab touchpad (exclusive access, system cursor stops moving) |
| Escape | Release grab |

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
```

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
