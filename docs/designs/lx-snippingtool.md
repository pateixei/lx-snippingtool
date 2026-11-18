# lx-snippingtool: Windows 11 Snipping Tool Clone for Linux

*   **Author:** lx-snippingtool contributors
*   **Status:** Proposed
*   **Date:** 2026-11-17

--------------------------------------------------------------------------------

## Overview

`lx-snippingtool` is a Rust desktop utility that brings the Windows 11 Snipping Tool workflow to Linux desktops, targeting everyday users migrating from Windows. The minimum supported baseline is Ubuntu 24.04+ and Fedora/RHEL 9+ equivalents running GNOME or KDE on Wayland and X11, distributed as native `.deb` and `.rpm` packages.

Phase 1 (v1) delivers screen capture (Rectangular, Freeform, Fullscreen, Delay Timer, and Window where permitted by the compositor/portal), a quick annotation editor (pen, highlighter, eraser, shapes, crop, and destructive pixel-level redaction), and a bounded local Snip History. Phase 2 will add screen recording (PipeWire + GStreamer/FFmpeg) and OCR text extraction/assisted redaction (Tesseract).

## The Problem

Users migrating from Windows 11 to Linux encounter fragmented screenshot tools across distributions and desktop environments:

1. **Inconsistent UX across GNOME and KDE:** Built-in tools (GNOME Screenshot, KDE Spectacle) differ in layout and keyboard flow across desktops, or omit Freeform capture and unified post-capture annotation.
2. **Wayland permission and clipboard constraints:** On Wayland, copied images vanish from the clipboard as soon as the capturing window exits unless a holder remains active, and GNOME Wayland lacks `wlr-data-control` / `ext-data-control-v1` (background clipboard protocols) and `GlobalShortcuts` portal support on Ubuntu 24.04 (GNOME 46).
3. **Heavyweight vs. minimal tools:** Full-featured tools often pull large runtime dependencies or X11-only assumptions, while minimal tools omit annotation, redaction, or capture history.

`lx-snippingtool` addresses this as a single-binary Rust application (`lx-snippingtool`) using `iced` (`tiny-skia` software renderer), `xdg-desktop-portal` (`ashpd`, the Rust portal client library), and a lightweight post-copy clipboard-holder lifecycle.

## Goals

*   **Familiar Windows 11 Capture Flow (Phase 1):** Support Rectangular, Freeform, Fullscreen, Delay Timer (3s/5s/10s), and Window capture (pre-freeze via portal picker or X11 window list) with low-latency (< 250 ms) freeze-frame selection.
*   **Built-in Annotation & Safe Redaction (Phase 1):** Provide Pen, Highlighter, Eraser, Shapes (rectangle, ellipse, arrow, line), Crop, and irreversible pixel-level Redaction with EXIF/PNG metadata stripped on export.
*   **Cross-Desktop Compatibility:** Run on Ubuntu 24.04+ and Fedora/RHEL 9+ equivalents across GNOME and KDE (Wayland and X11) via native `.deb` and `.rpm` packages.
*   **Persistent Clipboard After Exit:** Retain Wayland/X11 clipboard ownership after the editor/overlay closes by keeping a minimal clipboard-holder alive until selection is replaced.
*   **Bounded Snip History:** Store captured snips locally under `$XDG_DATA_HOME/lx-snippingtool/snips/` with automatic pruning beyond a configurable cap (default 50 items).
*   **Actionable Diagnostics:** Emit structured `tracing` logs to `stderr` and `journald` (`tracing-journald`) for portal and compositor troubleshooting.

## Non-Goals

*   **Phase 2 Features in v1:** Screen recording (PipeWire `ScreenCast` + GStreamer/FFmpeg) and OCR text extraction/assisted redaction (Tesseract) are deferred to Phase 2.
*   **System-Wide External Clipboard Manager:** Watching external clipboard copies from other applications (`wlr-data-control` / `Win+V` global history) is out of scope for v1; history is scoped to snips created by `lx-snippingtool`.
*   **Pure-Rust Reimplementation of Codecs/Protocols:** The project will not reimplement video encoders, OCR models, or Wayland compositor protocols from scratch.
*   **Bypassing Wayland Security:** No root/input-injection workarounds (`uinput`/`ydotool`) or private Mutter/KWin APIs to force unauthorized window lists.
*   **Legacy Distro Support:** Distributions older than Ubuntu 24.04 LTS or RHEL 9+ equivalents are out of scope.

## Open Questions

None.

## Non-Functional Requirements

*   **Security & Privacy:** Redaction boxes must irreversibly overwrite underlying RGBA pixels on export and strip all EXIF and PNG text chunks (`tEXt`, `zTXt`, `iTXt`). Local snip storage (`snips/`) must enforce `0700` directory permissions and `0600` file permissions.
*   **Performance & Latency:** Capture overlay startup (from `lx-snippingtool` trigger after screenshot acquisition to first rendered freeze-frame) must target < 250 ms on warm OS page/font cache using `tiny-skia` software rendering with a cached static background layer (avoiding `wgpu` shader-init latency).
*   **Concurrency & Quotas:** A single-instance lockfile (`$XDG_RUNTIME_DIR/lx-snippingtool.lock`) must prevent overlapping capture overlays on rapid double-presses. Snip History must prune oldest PNG files by mtime beyond `history_max_items` (default 50).

## Critical User Journeys (CUJs)

### CUJ 1: Quick Region Capture & Paste (Windows 11 `Win+Shift+S` Flow)

*   **Given**: The user is logged into GNOME or KDE (Wayland or X11) with `lx-snippingtool` installed via `.deb` or `.rpm` and bound to a shortcut (or launched via `.desktop`).
*   **When**: The user triggers capture, selects Rectangular (or Freeform) mode, and drags a region over the frozen screen overlay.
*   **Then**: The selected image is copied to the system clipboard (held by a minimal background clipboard-holder until replaced), saved to `snips/`, and a notification opens the annotation editor when clicked.

### CUJ 2: Annotate, Redact & Save/Copy

*   **Given**: A captured image is open in the `lx-snippingtool` annotation editor.
*   **When**: The user draws with the Pen/Highlighter, applies Redaction over sensitive text, crops the image, and clicks Copy or Save As.
*   **Then**: Redacted regions are permanently rasterized into the exported pixels, metadata is stripped, JPEG exports composite transparent regions onto an opaque white background, and the final image is copied to the clipboard or written to disk.

### CUJ 3: Delayed Capture & Snip History Recall

*   **Given**: The user needs to capture a transient menu or reuse an earlier snip.
*   **When**: The user sets a 3s/5s/10s delay timer before capture, or launches `lx-snippingtool --history`.
*   **Then**: The countdown completes in-process before invoking screen capture, or the user selects a previous snip from `snips/` and copies/reopens it.

## Technical Plan (Architecture)

### 1. Single-Binary Architecture & Pre-Implementation Spike

To eliminate cross-process D-Bus/`memfd` IPC complexity, avoid Delay Timer vs. capture ownership splits, and handle GNOME Wayland clipboard ownership uniformly, `lx-snippingtool` is a **single binary** organized into two Cargo crates (`lx-snip-core` library + `lx-snippingtool` binary):

```text
  [Trigger: DE Shortcut / .desktop / CLI (--mode, --delay, --history)]
                                  |
                                  v
                     +-------------------------+
                     |     lx-snippingtool     |
                     |  (Single-Instance Lock) |
                     +-------------------------+
                        |         |         |
       +----------------+         |         +----------------+
       v                          v                          v
  [Delay Pill]            [CaptureBackend]            [History View]
  (in-process)            (ashpd / x11rb)              (snips/*.png)
                                  |
                                  v
                       [Freeze-Frame Overlay]
                       (iced_layershell / winit)
                                  |
                                  v
                       [Annotation Editor] -> [Save As / Notification]
                                  |
                                  v (on Copy: drop UI & buffers)
                       [Minimal Clipboard Holder]
                       (stays alive until selection cancelled)
```

*   **Phase 0 Validation Spike (Required Before Full Overlay Implementation):** Validate `iced` (`winit` fullscreen per `wl_output`) vs. `iced_layershell` (`wlr-layer-shell`, a Wayland protocol supported on KDE/wlroots but absent on GNOME Mutter) on a mixed-DPI GNOME 46 Wayland setup (e.g., 4K @ 200% + 1080p @ 100%) and KDE Plasma 6 Wayland. Confirm exact `wl_output` / `zxdg_output_manager_v1` coordinate mapping against `org.freedesktop.portal.Screenshot`'s stitched PNG; if `winit` cannot align per-monitor windows reliably on GNOME Wayland, fall back to a custom `wayland-client` + `tiny-skia` surface for the GNOME overlay path.

### 2. Capture, Window Mode & Delay Timer Pipeline

1.  **In-Process Delay Timer:** When `--delay N` (3/5/10s) is active, `lx-snippingtool` displays a compact floating countdown pill *before* invoking `CaptureBackend`, keeping menus and hover states live until the timer expires.
2.  **Capture Backends (`CaptureBackend` trait):**
    *   **Wayland (`PortalCaptureBackend` via `ashpd`):** Calls `org.freedesktop.portal.Screenshot` (`interactive=false` when permitted, registering the unsandboxed `app_id` via `ashpd::register_host_app` / matching `.desktop` basename). PipeWire `ScreenCast` is deferred to Phase 2 screen recording.
    *   **X11 (`X11CaptureBackend` via `x11rb`):** Uses XShm screen capture (`shm`, `randr`, `xfixes` features) and `_NET_CLIENT_LIST` for native window geometry.
3.  **Window Mode UX:** On X11, Window mode uses `_NET_CLIENT_LIST` bounds inside the freeze overlay. On Wayland, Window mode delegates to `org.freedesktop.portal.Screenshot` (`interactive=true`) *before* freezing (via a pre-capture mode bar or `--mode window` trigger) rather than inside the already-frozen overlay, preventing the overlay from capturing itself.
4.  **Freeze-Frame Overlay (`iced` + `iced_layershell`):**
    *   `main.rs` dispatches between two explicit `iced` daemon runners (`iced_layershell::daemon` with `#[to_layer_message]` on KDE/wlroots Wayland, and `iced::daemon` on GNOME Wayland and X11) so `DelayPill -> Capturing -> Overlay -> Editor` share a single `winit::EventLoop` without recreation panics.
    *   Caches the pre-composited static frozen background image once on overlay open so pointer drag redraws (marquee/freeform lasso) only composite lightweight vector overlays on top.

### 3. Annotation Editor & Destructive Redaction

*   **Retained-Mode Scene (`AnnotationScene`):** Stores vector operations (`Pen`, `Highlighter`, `Shape` [Rect, Ellipse, Arrow, Line], `RedactBox`, and `CropRect`) in image-pixel coordinates independently of `iced` widgets, supporting Undo/Redo and stroke-level hit-testing for the Eraser.
*   **Export & Redaction Pipeline:** On Copy or Save As (`ashpd::desktop::file_chooser`), `tiny-skia` rasterizes the cropped base image and annotations into a flat RGBA buffer. `RedactBox` regions are painted as opaque solid fills directly onto the buffer (irreversibly destroying underlying pixels). PNG exports strip `tEXt`/`zTXt`/`iTXt` and EXIF; JPEG exports composite alpha onto an opaque white background before encoding.

### 4. Clipboard Persistence, Entry Points & Notifications

*   **Wayland/X11 Clipboard Holder:** When the user clicks Copy, `lx-snippingtool` releases the capture-overlay lockfile, drops its `iced` window/buffers, and retains only a minimal headless clipboard holder (`wl-clipboard-rs` on `data-control`-capable KDE/wlroots compositors; `x11rb` X11/XWayland selection holder on GNOME Wayland and X11) serving the exported PNG bytes until selection ownership is replaced.
*   **Entry Points & Shortcuts:** `.desktop` file (`io.github.lx_snippingtool.SnippingTool.desktop`), CLI flags (`--mode rect|freeform|window|full --delay N --history`), and optional `StatusNotifierItem` (`ksni` tray, omitted when no SNI host exists on vanilla Fedora GNOME). On Ubuntu 24.04 GNOME 46 (where `GlobalShortcuts` portal is absent), `README.md` and first-run onboarding guide the user to bind `lx-snippingtool --mode rect` in GNOME Settings -> Keyboard.
*   **Clickable Notification:** On capture copy, `lx-snippingtool` sends a desktop notification via `ashpd::notification` with an `"edit"` action; clicking it invokes `lx-snippingtool --editor <snip_path>` while the clipboard holder waits or exits cleanly.

### 5. Local Storage & Native Packaging (`.deb` / `.rpm`)

*   **Storage:**
    *   `$XDG_CONFIG_HOME/lx-snippingtool/config.toml`: User preferences (`default_mode`, `default_delay_secs`, `auto_copy`, `history_max_items`).
    *   `$XDG_DATA_HOME/lx-snippingtool/snips/<timestamp>_<uuid>.png`: Bounded Snip History (`0700` dir, `0600` files) pruned by mtime beyond `history_max_items` (default 50).
*   **Packaging (`.deb` and `.rpm`):** Single-package `[package.metadata.deb]` and `[package.metadata.generate-rpm]` in `crates/lx-snippingtool/Cargo.toml`, installing `/usr/bin/lx-snippingtool` and `/usr/share/applications/io.github.lx_snippingtool.SnippingTool.desktop`.
*   **Observability:** `tracing` + `tracing-journald` + `stderr` logging, plus `std::panic::set_hook` logging backtraces.

## Alternatives Considered

### Alternative A: `gtk4-rs` + `libadwaita` GUI Toolkit

*   **Pros**: Native `GtkWindow::fullscreen_on_monitor` for multi-monitor GNOME Wayland overlays, built-in `ashpd::WindowIdentifier` (`xdg_foreign`) parenting, and mature Cairo 2D drawing.
*   **Cons**: Heavy C FFI surface (`libgtk-4`, `libadwaita-1`, `glib`, `cairo`), slower `*-sys` builds, verbose `Rc<RefCell<...>>` / `glib::clone!` ergonomics, and Adwaita styling diverges from KDE Plasma Breeze.
*   **Reason for Rejection**: `iced` was chosen to keep the UI layer pure Rust with an Elm-style state model, built-in `Canvas` widget, MIT licensing, and uniform appearance across GNOME and KDE.

### Alternative B: `slint` GUI Toolkit

*   **Pros**: Separates `.slint` declarative markup from Rust logic with fast UI startup.
*   **Cons**: No built-in 2D vector canvas for freeform lasso, pen/highlighter strokes, and crop handles (requiring manual `tiny-skia` pixel blitting per mouse event), `winit` multi-monitor Wayland overlay limitations, and GPLv3 / commercial dual-licensing constraints.
*   **Reason for Rejection**: `iced` provides a native vector `Canvas` widget and MIT licensing without manual pixel-buffer blitting boilerplate.

### Alternative C: Two-Binary Split (`lx-snip-daemon` + `lx-snip-ui` via D-Bus)

*   **Pros**: Isolates UI/GPU crashes from a long-lived tray/shortcut process.
*   **Cons**: On Ubuntu 24.04 GNOME Wayland, `GlobalShortcuts`, `ksni` tray, and `wlr-data-control` are unavailable, making a 24/7 `systemd --user` daemon unable to hold Wayland clipboard ownership headlessly while adding D-Bus/`memfd` IPC and Delay Timer callback complexity.
*   **Reason for Rejection**: Collapsed to a single binary (`lx-snippingtool`) with a minimal post-copy clipboard-holder lifecycle.

### Alternative D: SQLite (`rusqlite`) + `history.json` Index

*   **Pros**: Atomic transactions and metadata indexing for future full-text search.
*   **Cons**: `rusqlite` adds a `libsqlite3` C dependency, and a `history.json` index adds schema-migration and corruption-recovery code for <=50 PNGs.
*   **Reason for Rejection**: Storing timestamped PNGs in `snips/` with mtime pruning eliminates index files and C database dependencies for v1.

### Alternative E: Flatpak as Primary Distribution Format

*   **Pros**: Sandboxed runtime isolates Phase 2 GStreamer/Tesseract ABI differences and enforces `app_id` for `xdg-desktop-portal`.
*   **Cons**: Larger runtime download overhead for users without GNOME/Freedesktop Flatpak runtimes installed, and sandbox friction with host CLI shortcuts.
*   **Reason for Rejection**: Native `.deb` (Ubuntu 24.04+) and `.rpm` (Fedora/RHEL 9+) packages were chosen for lighter installation and direct desktop integration.

### Alternative F: All-in-One v1 (Including Screen Recording & OCR)

*   **Pros**: Immediate parity with every Windows 11 Snipping Tool feature in the first release.
*   **Cons**: Pulling GStreamer/FFmpeg (with Fedora/RHEL patent-encumbered codec gaps) and Tesseract (`tessdata` packs) into v1 inflates packaging complexity and delays core capture/annotation stability.
*   **Reason for Rejection**: Screen recording and OCR are staged to Phase 2.

--------------------------------------------------------------------------------

## Detailed Implementation Plan

### Target Files to Create/Modify

#### 1. `Cargo.toml` (Create)

*   **Type / Scope**: Workspace Build Configuration
*   **Proposed Changes**: Define Cargo workspace with members `crates/lx-snip-core` and `crates/lx-snippingtool`, and pin shared dependency versions and features: `tokio = { version = "1", features = ["rt-multi-thread", "macros", "time", "process"] }`, `serde = { version = "1", features = ["derive"] }`, `serde_json = "1"`, `toml = "0.8"`, `tiny-skia = "0.11"`, `image = "0.25"`, `tracing = "0.1"`, `tracing-subscriber = { version = "0.3", features = ["env-filter"] }`, `tracing-journald = "0.3"`, `thiserror = "2"`, `uuid = { version = "1", features = ["v4"] }`, `clap = { version = "4", features = ["derive"] }`, `ashpd = "0.10"`, `zbus = "5"`, `wayland-client = "0.31"`, `wayland-protocols = { version = "0.32", features = ["client", "staging"] }`, `x11rb = { version = "0.13", features = ["shm", "randr", "xfixes"] }`, `wl-clipboard-rs = "0.9"`, `ksni = "0.2"`, `iced = { version = "0.13", default-features = false, features = ["tiny-skia", "canvas", "image", "tokio", "x11", "wayland", "wayland-dlopen", "wayland-decorations", "thread-pool"] }`, `iced_layershell = "0.12"`, and `directories = "5"`.

#### 2. `crates/lx-snip-core/Cargo.toml` (Create)

*   **Type / Scope**: Core Library Crate Manifest
*   **Proposed Changes**: Depend on `serde`, `serde_json`, `toml`, `tiny-skia`, `image`, `tracing`, `tracing-subscriber`, `tracing-journald`, `thiserror`, `uuid`, and `directories`.

#### 3. `crates/lx-snip-core/src/lib.rs` (Create)

*   **Type / Scope**: Core Library Entry Point
*   **Proposed Changes**: Re-export `config`, `storage`, `scene`, `export`, `platform`, and `telemetry` modules.

#### 4. `crates/lx-snip-core/src/config.rs` (Create)

*   **Type / Scope**: Configuration Schema
*   **Proposed Changes**: Define `CaptureMode` (`Rect`, `Freeform`, `Window`, `Fullscreen`), `AppConfig { default_mode: CaptureMode, default_delay_secs: u8, auto_copy: bool, history_max_items: usize }` (defaults: `Rect`, `0`, `true`, `50`), `load_or_default()`, and `save_atomic()` under `$XDG_CONFIG_HOME/lx-snippingtool/config.toml` via `directories::ProjectDirs`.

#### 5. `crates/lx-snip-core/src/storage.rs` (Create)

*   **Type / Scope**: Bounded PNG Snip History Store
*   **Proposed Changes**: Manage `$XDG_DATA_HOME/lx-snippingtool/snips/` with `0700` dir permissions and `0600` file permissions, saving `<unix_ms>_<uuid>.png`, listing entries sorted by mtime descending (`SnipEntry { path, created_at, byte_size }`), and pruning oldest files beyond `history_max_items`.

#### 6. `crates/lx-snip-core/src/scene.rs` (Create)

*   **Type / Scope**: Retained-Mode Annotation & Crop Model
*   **Proposed Changes**: Define `AnnotationScene` in image-pixel coordinates (`base_width`, `base_height`, `crop_rect: Option<Rect>`, `ops: Vec<AnnotationOp>`, `undo_stack`, `redo_stack`) where `AnnotationOp` covers `Pen { points, color, width }`, `Highlighter { points, color, width, opacity }`, `Shape { kind: ShapeKind, start, end, color, width }`, and `RedactBox { rect }`. Implement `erase_at(point, radius)` stroke-level hit-testing.

#### 7. `crates/lx-snip-core/src/export.rs` (Create)

*   **Type / Scope**: Destructive Rasterizer & Metadata Stripper
*   **Proposed Changes**: Define `ExportFormat { Png, Jpeg { quality: u8 } }` (default JPEG quality `90`). Implement `render_and_export(base_rgba, width, height, scene, format) -> Result<Vec<u8>>` using `tiny-skia` to apply `crop_rect`, composite strokes/shapes, paint `RedactBox` as opaque black fills directly into the pixel buffer, composite alpha onto white for JPEG, and encode without EXIF or PNG `tEXt`/`zTXt`/`iTXt` chunks.

#### 8. `crates/lx-snip-core/src/platform.rs` (Create)

*   **Type / Scope**: Platform Abstraction Traits
*   **Proposed Changes**: Define `CapturedImage { rgba: Vec<u8>, width: u32, height: u32, outputs: Vec<OutputGeometry> }`, `OutputGeometry { name, x, y, width, height, scale_factor }`, and traits `CaptureBackend` (`supports_window_mode`, `capture_desktop`, `capture_window`) and `ClipboardBackend` (`copy_png_and_hold`).

#### 9. `crates/lx-snip-core/src/telemetry.rs` (Create)

*   **Type / Scope**: Structured Logging & Panic Hook
*   **Proposed Changes**: Initialize `tracing_subscriber` with `tracing-journald` (omitting the journald layer without failing startup if the socket is absent) + `stderr` fmt layer (`RUST_LOG` override), and install `std::panic::set_hook` logging thread name and backtrace.

#### 10. `crates/lx-snippingtool/Cargo.toml` (Create)

*   **Type / Scope**: Binary Crate & Packaging Manifest
*   **Proposed Changes**: Declare `[[bin]] name = "lx-snippingtool"`, depend on `lx-snip-core`, `tokio`, `ashpd`, `zbus`, `wayland-client`, `wayland-protocols`, `x11rb`, `wl-clipboard-rs`, `ksni`, `iced`, `iced_layershell`, and `clap`. Include `[package.metadata.deb]` and `[package.metadata.generate-rpm]` with runtime deps (`libwayland-client0`, `libxkbcommon0`, `libxkbcommon-x11-0`, `libfontconfig1`, `xdg-desktop-portal` on Ubuntu 24.04+; `libwayland-client`, `libxkbcommon`, `libxkbcommon-x11`, `fontconfig`, `xdg-desktop-portal` on Fedora/RHEL 9+) and asset install rules for `/usr/bin/lx-snippingtool` and `/usr/share/applications/io.github.lx_snippingtool.SnippingTool.desktop`.

#### 11. `crates/lx-snippingtool/src/main.rs` (Create)

*   **Type / Scope**: CLI Entry Point, Lock & Runner Dispatch
*   **Proposed Changes**: Parse CLI flags (`--mode rect|freeform|window|full`, `--delay <secs>`, `--editor <path>`, `--history`, `--tray`). Acquire `$XDG_RUNTIME_DIR/lx-snippingtool.lock` only for capture-overlay invocations (`--editor` and `--history` bypass it), releasing the lock before entering the post-copy clipboard-holder loop. Dispatch to `iced_layershell::daemon` (KDE/wlroots) or `iced::daemon` (GNOME/X11) as a single `EventLoop` state machine (`DelayPill -> Capturing -> Overlay -> Editor`).

#### 12. `crates/lx-snippingtool/src/capture_wayland.rs` (Create)

*   **Type / Scope**: Wayland Portal Capture Backend
*   **Proposed Changes**: Implement `CaptureBackend` using `ashpd::desktop::screenshot::Screenshot` (`interactive=false` for desktop freeze-frame; `interactive=true` for pre-freeze `--mode window`), registering `io.github.lx_snippingtool.SnippingTool` via `ashpd::register_host_app` for unsandboxed permission persistence, and querying `wl_output` / `zxdg_output_manager_v1` bounds for mixed-DPI slicing.

#### 13. `crates/lx-snippingtool/src/capture_x11.rs` (Create)

*   **Type / Scope**: X11 Capture Backend
*   **Proposed Changes**: Implement `CaptureBackend` via `x11rb` XShm screen capture (swizzling BGRA to RGBA), `randr` per-monitor geometry, and `_NET_CLIENT_LIST` window bounds.

#### 14. `crates/lx-snippingtool/src/clipboard.rs` (Create)

*   **Type / Scope**: Post-Copy Clipboard Holder
*   **Proposed Changes**: Implement `ClipboardBackend` using `wl-clipboard-rs` on `data-control`-capable Wayland compositors (KDE/wlroots) and `x11rb` X11/XWayland selection ownership (`X11ClipboardHolder`) on GNOME Wayland and X11, ensuring copied images persist after the editor/overlay closes.

#### 15. `crates/lx-snippingtool/src/notification.rs` (Create)

*   **Type / Scope**: Clickable Desktop Notification
*   **Proposed Changes**: Send post-copy notification via `ashpd::notification` with `"edit"` action and listen for `receive_action_invoked` while the clipboard holder is active, spawning `lx-snippingtool --editor <snip_path>` via `std::process::Command` when clicked.

#### 16. `crates/lx-snippingtool/src/overlay.rs` (Create)

*   **Type / Scope**: Multi-Monitor Freeze-Frame Selection Overlay
*   **Proposed Changes**: Render the cached frozen desktop background per output with normalized fractional DPI scaling, centered top floating pill toolbar (Rect, Freeform lasso, Window [disabled with tooltip on Wayland post-freeze], Fullscreen, Delay), and selection marquee.

#### 17. `crates/lx-snippingtool/src/editor.rs` (Create)

*   **Type / Scope**: Annotation Editor Window
*   **Proposed Changes**: Render `iced::widget::Canvas` backed by `AnnotationScene`, toolbar (Pen, Highlighter, Eraser, Shapes, Redact, Crop, Undo/Redo, Copy, Save As via `ashpd::desktop::file_chooser`), and export trigger calling `lx-snip-core::export`.

#### 18. `crates/lx-snippingtool/src/history_view.rs` & `crates/lx-snippingtool/src/tray.rs` (Create)

*   **Type / Scope**: Snip History UI & Optional SNI Tray
*   **Proposed Changes**: Render bounded `snips/` thumbnails with Copy/Edit/Delete actions; register `ksni::Tray` when `--tray` is invoked, logging at `debug` and continuing as a no-op if no SNI host is present.

#### 19. `packaging/io.github.lx_snippingtool.SnippingTool.desktop` (Create)

*   **Type / Scope**: Desktop Entry
*   **Proposed Changes**: Define `Exec=lx-snippingtool --mode rect`, `StartupWMClass=lx-snippingtool`, and desktop actions (`New Snip`, `Snip History`).

#### 20. `crates/lx-snip-core/tests/scene_export_tests.rs` & `crates/lx-snip-core/tests/storage_tests.rs` (Create)

*   **Type / Scope**: Core Unit & Integration Tests
*   **Proposed Changes**: Test `AnnotationScene` undo/redo, stroke eraser hit-testing, crop + destructive `RedactBox` pixel overwrite, JPEG alpha-to-white compositing, metadata stripping, `0600`/`0700` permissions, and mtime pruning.

#### 21. `README.md` (Create)

*   **Type / Scope**: Documentation
*   **Proposed Changes**: Document build prerequisites (`pkg-config`, `libxkbcommon-dev`, `libfontconfig1-dev`), `.deb`/`.rpm` installation, GNOME 46 manual shortcut binding (`Super+Shift+S` / `Print`), and Phase 0 spike instructions.

### Verification Plan

*   **Phase 0 Spike**: Run minimal `iced` / `iced_layershell` multi-monitor mixed-DPI overlay test on Ubuntu 24.04 (GNOME 46 Wayland) and Fedora 40+ (KDE Plasma 6 Wayland) to confirm coordinate alignment.
*   **Compilation/Build**: `cargo build --workspace --release`, `cargo deb --manifest-path crates/lx-snippingtool/Cargo.toml`, and `cargo generate-rpm -p crates/lx-snippingtool`.
*   **Unit Tests**: `cargo test --workspace` covering `scene_export_tests.rs`, `storage_tests.rs`, and `config.rs`.
*   **Static Analysis & Linting**: `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check`.
*   **Test Coverage Requirement**: >= 85% line coverage on `lx-snip-core` measured via `cargo-llvm-cov`.
*   **E2E/Integration**: Manual verification matrix on Ubuntu 24.04 (GNOME Wayland + X11) and Fedora 40+ (GNOME + KDE Plasma Wayland):
    *   Verify double-pressing shortcut causes the second invocation to exit without opening a duplicate overlay (`$XDG_RUNTIME_DIR/lx-snippingtool.lock`).
    *   Verify Rectangular, Freeform, Fullscreen, Window (X11 + Wayland pre-freeze), and Delay 3s captures persist in clipboard after the editor closes.
    *   Verify clicking the post-capture notification opens the annotation editor.
    *   Verify `RedactBox` export leaves zero original pixels or EXIF/`tEXt`/`zTXt`/`iTXt` chunks (inspected via `exiftool`).

--------------------------------------------------------------------------------

*Drafted with EGM*
