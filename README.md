# lx-snippingtool

A Windows 11 Snipping Tool-inspired screen capture, annotation, redaction, and screen recording utility for Linux desktops (Ubuntu 24.04+ and Fedora/RHEL 9+ equivalents on GNOME and KDE, Wayland and X11).

## Features

- **Capture Modes:** Rectangular, Freeform lasso, Window (X11 window picking / Wayland portal picker), Fullscreen, Delay Timer (3s / 5s / 10s), and Screen Recording (`Record`).
- **Screen Recording (Video + Audio):** Region-cropped MP4 (H.264) or WebM (VP8/VP9) capture via PipeWire `ScreenCast` (Wayland) or `ximagesrc` (X11) with independent toggles for system audio (`@DEFAULT_MONITOR@`) and default microphone (`@DEFAULT_SOURCE@`) mixed in real time via GStreamer `audiomixer`.
- **Annotation & Destructive Redaction:** Pen, Highlighter, Stroke Eraser, Shapes (Rectangle, Ellipse, Arrow, Line), Crop, and irreversible pixel-level Redaction with EXIF and PNG text metadata stripped on export.
- **Persistent Clipboard:** Keeps copied PNG images available on Wayland and X11 after the editor closes.
- **Bounded Snip History:** Stores snips locally under `$XDG_DATA_HOME/lx-snippingtool/snips/` (`0700` directory, `0600` files) with automatic mtime-based pruning.

## Build Prerequisites

### Ubuntu / Debian (24.04+)

```bash
sudo apt install build-essential pkg-config libxkbcommon-dev libxkbcommon-x11-dev \
  libfontconfig1-dev libwayland-dev libpipewire-0.3-dev libclang-dev xdg-desktop-portal \
  gstreamer1.0-tools gstreamer1.0-pipewire gstreamer1.0-plugins-good
```

*(Install `gstreamer1.0-plugins-ugly` if you prefer H.264/MP4 encoding over VP8/VP9 WebM.)*

### Fedora / RHEL (9+)

```bash
sudo dnf install gcc pkgconf-pkg-config libxkbcommon-devel libxkbcommon-x11-devel \
  fontconfig-devel wayland-devel pipewire-devel clang-devel xdg-desktop-portal \
  gstreamer1-plugins-good pipewire-gstreamer
```

## Building & Testing

```bash
cargo build --workspace --release
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Packaging (`.deb` / `.rpm`)

```bash
# Debian / Ubuntu package
cargo install cargo-deb
cargo deb --manifest-path crates/lx-snippingtool/Cargo.toml

# Fedora / RHEL package
cargo install cargo-generate-rpm
cargo build --release -p lx-snippingtool
cargo generate-rpm -p crates/lx-snippingtool
```

Install the resulting `.deb` with `sudo apt install ./target/debian/lx-snippingtool_*.deb` or `.rpm` with `sudo dnf install ./target/generate-rpm/lx-snippingtool-*.rpm`.

## Keyboard Shortcut Setup (GNOME)

You can register `Ctrl+Shift+S` automatically as a custom GNOME shortcut:

```bash
lx-snippingtool --install-shortcut
```

Or configure it manually under **Settings → Keyboard → View and Customize Shortcuts → Custom Shortcuts** pointing to `lx-snippingtool --mode rect`.

## CLI Usage

```text
lx-snippingtool [--mode rect|freeform|window|full|record] [--delay <secs>] [--editor <path>] [--history] [--tray] [--install-shortcut]
```
