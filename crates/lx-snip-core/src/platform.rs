//! Platform abstraction traits for screen capture and clipboard ownership.

use std::future::Future;
use std::pin::Pin;

use thiserror::Error;

/// Boxed future type used by object-safe async platform traits.
pub type PlatformFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, PlatformError>> + Send + 'a>>;

/// Logical/physical geometry of a single display output.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputGeometry {
    /// Compositor or RandR output name (e.g. `DP-1`, `eDP-1`).
    pub name: String,
    /// X origin in global compositor coordinate space.
    pub x: i32,
    /// Y origin in global compositor coordinate space.
    pub y: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Display scale factor (e.g. `1.0`, `2.0`).
    pub scale_factor: f64,
}

/// Raw captured desktop or window image in straight RGBA8 format.
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedImage {
    /// Straight RGBA8 pixel buffer (`width * height * 4` bytes).
    pub rgba: Vec<u8>,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Output geometries covering the captured surface.
    pub outputs: Vec<OutputGeometry>,
}

/// Errors returned by [`CaptureBackend`] and [`ClipboardBackend`] implementations.
#[derive(Debug, Error)]
pub enum PlatformError {
    /// Requested capability is unsupported on the active compositor/session.
    #[error("unsupported platform operation: {0}")]
    Unsupported(String),
    /// Screen or window capture failed.
    #[error("capture failed: {0}")]
    CaptureFailed(String),
    /// Clipboard ownership or transfer failed.
    #[error("clipboard failed: {0}")]
    ClipboardFailed(String),
    /// User cancelled the portal or selection prompt.
    #[error("operation cancelled by user")]
    Cancelled,
}

/// Abstraction over Wayland portal and X11 screen capture backends.
pub trait CaptureBackend: Send + Sync {
    /// Returns `true` if the backend supports enumerating/capturing individual windows
    /// inside the freeze-frame overlay (X11) rather than requiring pre-freeze portal selection.
    fn supports_window_mode(&self) -> bool;

    /// Captures the full desktop across all active outputs.
    fn capture_desktop(&self) -> PlatformFuture<'_, CapturedImage>;

    /// Captures a single window (e.g. via interactive portal on Wayland or `_NET_CLIENT_LIST` on X11).
    fn capture_window(&self) -> PlatformFuture<'_, CapturedImage>;
}

/// Abstraction over Wayland `data-control` and X11/XWayland clipboard holders.
pub trait ClipboardBackend: Send + Sync {
    /// Publishes `png_bytes` as `image/png` on the clipboard and holds ownership
    /// until selection ownership is replaced or lost.
    fn copy_png_and_hold(&self, png_bytes: Vec<u8>) -> PlatformFuture<'_, ()>;
}
